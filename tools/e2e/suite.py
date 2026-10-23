#!/usr/bin/env python3.11
"""Run the complete registered E2E matrix with isolated state and owned cleanup."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import shutil
import subprocess
import sys
import threading
import time
from rekor_fixture import rekor_url

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_name("suite.json")


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def file_digest(path):
    with path.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256")
    return digest.hexdigest()


def registered_cases():
    manifest = json.loads(MANIFEST.read_text())
    cases = {case["id"]: case for case in manifest["cases"]}
    if len(cases) != len(manifest["cases"]):
        raise ValueError("duplicate E2E case IDs")
    for case in cases.values():
        if any(dependency not in cases for dependency in case.get("depends", [])):
            raise ValueError("unknown E2E dependency")
    return cases


def select_cases(cases, groups, names):
    selected = set(names)
    if not names:
        selected.update(name for name, case in cases.items()
                        if "full" in groups or set(groups).intersection(case["groups"]))
    if selected - cases.keys():
        raise ValueError("unknown case: " + ", ".join(sorted(selected - cases.keys())))
    def include(name, visiting):
        if name in visiting:
            raise ValueError("cyclic E2E dependencies")
        for dependency in cases[name].get("depends", []):
            include(dependency, visiting | {name})
            selected.add(dependency)
    for name in list(selected):
        include(name, set())
    return {name: case for name, case in cases.items() if name in selected}


def recover_cleanup(work, environment, logfile):
    """Use only the durable resource registry created by this case."""
    registry = work / "destroy-targets.json"
    ca = environment.get("CA_BIN", str(ROOT / "target/debug/confidential-agent"))
    ok = True
    recovered = False
    for target in json.loads(registry.read_text()) if registry.exists() else []:
        recovered = True
        state_path = Path(target["state_dir"]) / "services" / target["service"] / "state.json"
        if not state_path.exists():
            continue
        state = json.loads(state_path.read_text())
        # Formal destroy may already have revoked the last Trustee membership.
        # Its completed state must not re-enter provider recovery on a retry.
        if state.get("phase") == "deleted":
            continue
        deploy = state.get("deploy") or {}
        # Build records contain planned names before any infrastructure exists.
        # Completed local builds use image rm; an attempted deploy or a published
        # image must retain the normal cloud cleanup path and its failure checks.
        observed_fields = ("terraform_dir", "image_source", "bucket", "instance_id",
                           "security_group_id", "private_ip", "public_ip", "published_image_id",
                           "instance_ids", "instance_private_ips", "instance_public_ips", "slb_vip")
        local_build = (state.get("phase") == "built"
                       and not (state.get("build") or {}).get("published")
                       and not (Path(target["state_dir"]) / "trustee/state.json").exists()
                       and not any(deploy.get(field) for field in observed_fields))
        operation = (["image", "rm", target["service"], "--force"] if local_build
                     else ["destroy", target["service"]])
        command = [ca, "--state-dir", target["state_dir"], *operation]
        try:
            result = subprocess.run(command, env=environment, stdout=logfile, stderr=logfile,
                                    timeout=1800, start_new_session=True)
            ok = result.returncode == 0 and ok
        except (OSError, subprocess.TimeoutExpired):
            ok = False
    ledger = work / "cloud-resource-ledger.json"
    if ledger.exists():
        recovered = True
        command = [sys.executable, str(ROOT / "tools/e2e/cleanup_snapshots.py"),
                   "cleanup", str(ledger), environment.get("E2E_REGION", "cn-beijing")]
        try:
            result = subprocess.run(command, env=environment, stdout=logfile, stderr=logfile,
                                    timeout=1800, start_new_session=True)
            ok = result.returncode == 0 and ok
        except (OSError, subprocess.TimeoutExpired):
            ok = False
    # Legacy trustee-duo keeps its fixture at the case root; newer cases use
    # a trustee/ child. Retain either server when service cleanup needs retry.
    for trustee in [work / "trustee", work]:
        if not (trustee / "trustee-infra-ids.json").exists():
            continue
        recovered = True
        if not ok:
            continue
        trustee_environment = dict(environment, E2E_TRUSTEE_WORK_DIR=str(trustee),
                                   E2E_STATE_DIR=str(work / "state"))
        try:
            result = subprocess.run(["bash", str(ROOT / "tools/e2e/trustee_fixture.sh"), "destroy"],
                                    env=trustee_environment, stdout=logfile, stderr=logfile,
                                    timeout=2400, start_new_session=True)
            ok = result.returncode == 0 and ok
        except (OSError, subprocess.TimeoutExpired):
            ok = False
    return recovered and ok


def remove_local_images(work, environment, logfile):
    """Remove large local artifacts only after their cloud deployment is deleted."""
    registry = work / "destroy-targets.json"
    if not registry.exists():
        return
    ca = environment.get("CA_BIN", str(ROOT / "target/debug/confidential-agent"))
    evidence = []
    for target in json.loads(registry.read_text()):
        directory = Path(target["state_dir"]) / "services" / target["service"]
        path = directory / "state.json"
        if not path.exists():
            continue
        state = json.loads(path.read_text())
        if state.get("phase") != "deleted":
            continue
        record = {"service": target["service"], "deployment": state["deploy"]}
        manifest = directory / "manifest.json"
        if manifest.exists():
            record["image"] = json.loads(manifest.read_text()).get("image")
        evidence.append(record)
        if state.get("build", {}).get("remote") is True:
            command = ["forget", target["service"]]
        else:
            command = ["image", "rm", target["service"], "--force"]
        result = subprocess.run([ca, "--state-dir", target["state_dir"], *command],
                                env=environment, stdout=logfile, stderr=logfile, timeout=300)
        if result.returncode:
            raise RuntimeError("deleted deployment's local image cleanup failed")
    write_json(work / "deleted-deployments.json", evidence)


class Suite:
    def __init__(self, work, cases, environment, cpu_jobs, gpu_jobs):
        self.work, self.cases, self.environment = work, cases, environment
        self.results = {}
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.processes = {}
        self.pools = {"cpu": concurrent.futures.ThreadPoolExecutor(cpu_jobs),
                      "gpu": concurrent.futures.ThreadPoolExecutor(gpu_jobs)}

    def terminate(self, *_):
        self.stop.set()
        with self.lock:
            for process in self.processes.values():
                if process.poll() is None:
                    try:
                        os.killpg(process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        # A case may exit between poll() and killpg(). Continue
                        # interrupting the remaining cases and retain results.
                        pass

    def execute(self, name, case):
        if self.stop.is_set():
            return {"status": "blocked", "reason": "suite interrupted", "cleanup_complete": True}
        work = self.work / name
        work.mkdir(mode=0o700)
        environment = dict(self.environment)
        if case.get("pool") == "gpu":
            environment["E2E_INSTANCE_TYPE"] = (environment.get("E2E_GPU_INSTANCE_TYPE")
                                                or "ecs.gn8v-tee.4xlarge")
            if environment.get("E2E_GPU_ZONE_ID"):
                environment["E2E_ZONE_ID"] = environment["E2E_GPU_ZONE_ID"]
        environment.update(case.get("environment", {}))
        environment.update(E2E_WORK_DIR=str(work), E2E_STATE_DIR=str(work / "state"),
                           E2E_SUITE_DIR=str(self.work), E2E_CASE_ID=name,
                           E2E_RUN_ID=time.strftime("%Y%m%d%H%M%S") + "-" + secrets.token_hex(6),
                           E2E_DESTROY_ON_SUCCESS="1", E2E_DESTROY_ON_FAILURE="1",
                           E2E_SKIP_BUILD="0", E2E_SKIP_DEPLOY="0")
        write_json(work / "run-identity.json", {"case": name, "run_id": environment["E2E_RUN_ID"]})
        for key in case.get("requires", []):
            if not environment.get(key):
                return {"status": "blocked", "reason": "missing " + key, "cleanup_complete": True}
        for command in case.get("required_commands", []):
            if command == "cargo" and environment.get("E2E_SKIP_CARGO_BUILD") == "1":
                continue
            if not shutil.which(command, path=environment.get("PATH")):
                return {"status": "blocked", "reason": "missing required command: " + command, "cleanup_complete": True}
        started = time.time()
        timed_out = False
        with (work / "case.private.log").open("wb") as log:
            command = [str(ROOT / argument) if argument.startswith("tools/") else argument
                       for argument in case["command"]]
            try:
                process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=log,
                                           stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
                with self.lock:
                    self.processes[name] = process
                try:
                    code = process.wait(timeout=case.get("timeout", 14400))
                except subprocess.TimeoutExpired:
                    timed_out = True
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        code = process.wait(timeout=1800)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        code = process.wait()
                finally:
                    with self.lock:
                        self.processes.pop(name, None)
            except OSError as error:
                return {"status": "failed", "reason": str(error), "cleanup_complete": False}
            result_path = work / "case-result.json"
            try:
                result = json.loads(result_path.read_text()) if result_path.exists() else {}
            except (ValueError, OSError):
                result = {"passed": False, "reason": "case result is missing or invalid"}
            deferred = case.get("retains_fixtures") and result.get("cleanup_deferred") is True
            cleaned = result.get("cleanup_complete") is True
            if not cleaned and not deferred:
                cleaned = recover_cleanup(work, environment, log)
            if cleaned and not deferred and not case.get("retains_local_images"):
                try:
                    remove_local_images(work, environment, log)
                except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
                    result["local_cleanup_error"] = str(error)
                    cleaned = False
            passed = code == 0 and result.get("passed") is True and (cleaned or deferred) and not timed_out
            result.update(status="passed" if passed else "failed", exit_code=code,
                          elapsed_seconds=round(time.time() - started, 2),
                          timed_out=timed_out, cleanup_complete=cleaned)
            write_json(work / "case-result.json", result)
            return result

    def producers_entered_builds(self):
        # Start every shared producer before unrelated cases can take the build
        # slots. A producer's STS/init work may finish later than an ordinary
        # case's setup, so submitting its future first is not sufficient.
        for name, case in self.cases.items():
            if not case.get("retains_fixtures") or case.get("depends") or name in self.results:
                continue
            entered = False
            for evidence in (self.work / name).glob("*slot*.timing.json"):
                try:
                    entered = json.loads(evidence.read_text()).get("stage") == "build-slot-wait"
                except (OSError, ValueError):
                    continue
                if entered:
                    break
            if not entered:
                return False
        return True

    def run(self):
        pending = dict(self.cases)
        running = {}
        try:
            while pending or running:
                followers = {name: sum(name in candidate.get("depends", []) for candidate in self.cases.values())
                             for name in pending}
                for name, case in sorted(list(pending.items()), key=lambda item: -followers[item[0]]):
                    dependencies = case.get("depends", [])
                    if any(dependency in self.results and self.results[dependency]["status"] != "passed"
                           for dependency in dependencies):
                        self.results[name] = {"status": "blocked", "reason": "dependency failed", "cleanup_complete": True}
                        del pending[name]
                    elif self.stop.is_set():
                        self.results[name] = {"status": "blocked", "reason": "suite interrupted", "cleanup_complete": True}
                        del pending[name]
                    elif all(dependency in self.results for dependency in dependencies):
                        if not case.get("retains_fixtures") and not self.producers_entered_builds():
                            continue
                        pool = self.pools[case.get("pool", "cpu")]
                        running[pool.submit(self.execute, name, case)] = name
                        del pending[name]
                if not running:
                    if pending:
                        raise ValueError("E2E dependency graph cannot progress")
                    break
                finished, _ = concurrent.futures.wait(running, timeout=1,
                                                      return_when=concurrent.futures.FIRST_COMPLETED)
                for future in finished:
                    name = running.pop(future)
                    try:
                        self.results[name] = future.result()
                    except Exception as error:
                        self.results[name] = {"status": "failed", "reason": str(error), "cleanup_complete": False}
                    print(name + ": " + self.results[name]["status"], flush=True)
                    write_json(self.work / "results.json", self.results)
        finally:
            for pool in self.pools.values():
                pool.shutdown(wait=True)
        producers = {name: result for name, result in self.results.items()
                     if self.cases[name].get("retains_fixtures") and
                     (result.get("cleanup_deferred") or result.get("cleanup_complete") is False)}
        if producers:
            lease = self.environment.get("E2E_FIXTURE_CONSUMER_LEASE")
            deadline = time.monotonic() + int(self.environment.get("E2E_FIXTURE_LEASE_TIMEOUT_SEC", "28800"))
            if lease and Path(lease).exists():
                print("Waiting for the independent signed-image consumers to finish", flush=True)
                for producer in producers.values():
                    producer["external_consumers_pending"] = True
                write_json(self.work / "results.json", self.results)
                while Path(lease).exists():
                    if self.stop.is_set() or time.monotonic() >= deadline:
                        for producer in producers.values():
                            producer.update(status="failed", cleanup_complete=False,
                                            reason="independent consumers still retain the signed fixtures")
                        write_json(self.work / "results.json", self.results)
                        return False
                    self.stop.wait(1)
                for producer in producers.values():
                    producer["external_consumers_pending"] = False
        for name, producer in producers.items():
            # A failed consumer may still need its image for cleanup/recovery.
            # Keep only that producer's fixtures; unrelated producers can clean.
            def depends_on(candidate, dependency):
                return any(parent == dependency or depends_on(parent, dependency)
                           for parent in self.cases[candidate].get("depends", []))
            if any(result.get("cleanup_complete") is not True and depends_on(consumer, name)
                   for consumer, result in self.results.items() if consumer != name):
                producer.update(status="failed", cleanup_complete=False,
                                reason="consumer cleanup is incomplete; shared fixtures retained")
                continue
            environment = dict(self.environment, E2E_WORK_DIR=str(self.work / name),
                               E2E_STATE_DIR=str(self.work / name / "state"))
            command = self.cases[name].get("cleanup_command", [sys.executable, "tools/e2e/publish_fixtures.py", "--cleanup"])
            command = [str(ROOT / part) if part.startswith("tools/") else part for part in command]
            with (self.work / name / "fixture-cleanup.private.log").open("wb") as log:
                try:
                    result = subprocess.run(command,
                                            env=environment, stdout=log, stderr=log, timeout=7200)
                    producer["cleanup_complete"] = result.returncode == 0
                    producer["cleanup_deferred"] = False
                    if result.returncode:
                        producer["status"] = "failed"
                except (OSError, subprocess.TimeoutExpired):
                    producer.update(status="failed", cleanup_complete=False)
            write_json(self.work / "results.json", self.results)
        # Persist dependency failures too, including the final iteration where
        # no consumer process was launched and no future completed.
        write_json(self.work / "results.json", self.results)
        return all(result["status"] == "passed" and result["cleanup_complete"]
                   for result in self.results.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--suite", choices=["full", "legacy", "reuse", "inference", "platform"], default="full")
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--gpu-jobs", type=int, default=2)
    parser.add_argument("--build-jobs", type=int, default=2,
                        help="Maximum concurrent image builds on this runner, independent of cloud case jobs")
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--fixture-consumer-lease", type=Path,
                        help="Retain publisher images until this independent consumer lease is removed")
    args = parser.parse_args()
    if args.jobs < 1 or args.gpu_jobs < 1 or args.build_jobs < 1:
        parser.error("concurrency must be positive")
    cases = select_cases(registered_cases(), [args.suite], args.case)
    if args.list or args.dry_run:
        print(json.dumps(cases, indent=2))
        return
    work = (args.work_dir or ROOT / ".tmp/e2e" / ("suite-" + time.strftime("%Y%m%d%H%M%S"))).resolve()
    work.mkdir(parents=True, mode=0o700, exist_ok=False)
    os.umask(0o077)
    pins = json.loads((ROOT / "software-versions.json").read_text())
    environment = dict(os.environ)
    environment.setdefault("E2E_BUILD_SLOT_DIR", str(work / "build-slots"))
    environment["E2E_BUILD_JOBS"] = str(args.build_jobs)
    if Path("/usr/libexec/shelter/bin/terraform").is_file():
        environment["PATH"] = environment.get("PATH", os.defpath) + os.pathsep + "/usr/libexec/shelter/bin"
    if args.fixture_consumer_lease:
        environment["E2E_FIXTURE_CONSUMER_LEASE"] = str(args.fixture_consumer_lease.resolve())
    environment.update(DASHSCOPE_MODEL=pins["bailian_model"], E2E_OPENCLAW_VERSION=pins["openclaw"],
                       E2E_OPENCLAW_NODE_VERSION=pins["node"], E2E_CLI_AGENT_NODE_VERSION=pins["node"],
                       E2E_CODEX_VERSION=pins["codex"], E2E_CLAUDE_CODE_VERSION=pins["claude-code"],
                       E2E_HERMES_COMMIT=pins["hermes"], E2E_OPENCLAW_VLLM_RUN_CLOUD="1",
                       E2E_MATRIX_REAL_CLOUD="1")
    metadata = {"schema_version": "confidential-agent/e2e-suite/v1", "software_versions": pins,
                "cases": list(cases), "started_at": time.time(), "binary_digests": {},
                "fixture_cases": [name for name, case in cases.items() if case.get("kind") == "fixture"],
                "rekor_url": rekor_url(),
                "build_jobs": args.build_jobs, "build_slot_dir": environment["E2E_BUILD_SLOT_DIR"]}
    for name in ["CA_BIN", "CA_AGENTD_BIN", "CA_GATEWAY_BIN", "CA_PEP_BIN", "CA_SHELTER_BIN"]:
        path = Path(environment.get(name, "/missing"))
        if path.is_file():
            metadata["binary_digests"][name] = file_digest(path)
    write_json(work / "metadata.json", metadata)
    suite = Suite(work, cases, environment, args.jobs, args.gpu_jobs)
    signal.signal(signal.SIGINT, suite.terminate)
    signal.signal(signal.SIGTERM, suite.terminate)
    passed = suite.run()
    metadata.update(finished_at=time.time())
    metadata["elapsed_seconds"] = round(metadata["finished_at"] - metadata["started_at"], 2)
    write_json(work / "metadata.json", metadata)
    rows = [f"Suite elapsed: {metadata['elapsed_seconds']} seconds (includes fixture cleanup).", "",
            "| Case | Kind | Result | Seconds | Cleanup |", "|---|---|---|---|---|"]
    rows += [f"| {name} | {cases[name].get('kind', 'test')} | {result['status']} | {result.get('elapsed_seconds', '')} | {result['cleanup_complete']} |"
             for name, result in suite.results.items()]
    (work / "report.md").write_text("\n".join(rows) + "\n")
    print("E2E suite " + ("passed" if passed else "incomplete") + "; results: " + str(work))
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
