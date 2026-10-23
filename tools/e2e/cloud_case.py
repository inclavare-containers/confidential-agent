"""Shared lifecycle and private evidence for cloud matrix cases."""
import base64
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import ipaddress
import time
from resource_ledger import merge_ledgers, read_ledger, record_service
from command_runner import build_slot
from rekor_fixture import configure_spec

ROOT = Path(__file__).resolve().parents[2]


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def run(command, evidence, timeout=1800, check=True, environment=None):
    evidence = Path(evidence)
    evidence.parent.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen([str(value) for value in command], stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, stdin=subprocess.DEVNULL,
                               env=environment, start_new_session=True)
    started_at, started = time.time(), time.monotonic()
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=60)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
        evidence.with_suffix(".out").write_bytes(stdout)
        evidence.with_suffix(".err").write_bytes(stderr)
        raise RuntimeError(evidence.name + " timed out; private diagnostics retained") from None
    except BaseException:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                stdout, stderr = process.communicate(timeout=60)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                stdout, stderr = process.communicate()
            evidence.with_suffix(".out").write_bytes(stdout)
            evidence.with_suffix(".err").write_bytes(stderr)
        raise
    finally:
        # Only the evidence label and timings are public-safe; command arguments
        # and environment can contain credentials and stay in private evidence.
        write_json(evidence.with_suffix(".timing.json"), {
            "label": evidence.name, "started_at": started_at,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "exit_code": process.returncode, "timed_out": timed_out})
    evidence.with_suffix(".out").write_bytes(stdout)
    evidence.with_suffix(".err").write_bytes(stderr)
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    if check and result.returncode:
        raise RuntimeError(evidence.name + " failed; private diagnostics retained")
    return result


def eventually(probe, timeout=300, interval=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if probe():
                return
        except (OSError, ValueError, RuntimeError, AssertionError):
            pass
        time.sleep(interval)
    raise RuntimeError("condition did not converge within its readiness budget")


def release_selection(directory, region):
    directory = Path(directory).resolve()
    envelope = json.loads((directory / "image-release.json").read_text())
    release = json.loads(base64.b64decode(envelope["payload"]))
    if not any(value["region"] == region for value in release["images"]):
        raise ValueError("signed release has no image mapping for region " + region)
    return {"release": str(directory / "image-release.json"),
            "trusted_key": str(directory / "publisher.pem"),
            "manifest": release["manifest"]["manifest_digest"]}


class Case:
    def __init__(self):
        os.umask(0o077)
        self.work = Path(os.environ["E2E_WORK_DIR"]).resolve()
        self.work.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.state = Path(os.environ.get("E2E_STATE_DIR", str(self.work / "state"))).resolve()
        self.suite = Path(os.environ.get("E2E_SUITE_DIR", str(self.work.parent))).resolve()
        self.region = os.environ.get("E2E_REGION", "cn-beijing")
        self.ca = os.environ.get("CA_BIN", str(ROOT / "target/debug/confidential-agent"))
        self.resources = []
        self.tunnels = []
        self.checks = {}
        self.trustee_work = None
        self.operator_cidr = None
        self.old_handlers = {}
        self.monitor_stop = threading.Event()
        self.monitor_thread = None
        self.defer_cleanup = False

    def command(self, label, *arguments, state=None, **options):
        if arguments and arguments[0] in {"build", "deploy"} and "--spec" in arguments:
            configure_spec(arguments[arguments.index("--spec") + 1])
        if arguments and arguments[0] == "build":
            with build_slot(self.work / (label + "-slot.timing.json")):
                return run([self.ca, "--state-dir", state or self.state, *arguments], self.work / label, **options)
        return run([self.ca, "--state-dir", state or self.state, *arguments], self.work / label, **options)

    def api(self, product, action, **parameters):
        arguments = ["aliyun", "--region", self.region, product, action, "--RegionId", self.region]
        for name, value in parameters.items():
            arguments += ["--" + name, json.dumps(value) if isinstance(value, (dict, list)) else str(value)]
        return json.loads(run(arguments, self.work / (action + "-" + str(time.time_ns()))).stdout)

    def operator(self):
        cidr = os.environ.get("E2E_ALLOWED_CIDR")
        if not cidr:
            import urllib.request
            cidr = urllib.request.urlopen("https://ipinfo.io/ip", timeout=30).read().decode().strip() + "/32"
        self.command("operator", "peering", "add", "--role", "operator", "--cidr", cidr, "--label", "suite-operator")
        self.operator_cidr = cidr
        return cidr

    def register(self, service, state=None):
        target = {"service": service, "state_dir": str(state or self.state)}
        if target not in self.resources:
            self.resources.append(target)
            write_json(self.work / "destroy-targets.json", self.resources)

    def trustee_action(self, action):
        environment = dict(os.environ, E2E_TRUSTEE_WORK_DIR=str(self.trustee_work),
                           E2E_STATE_DIR=str(self.state), E2E_ALLOWED_CIDR=self.operator_cidr)
        return run(["bash", ROOT / "tools/e2e/trustee_fixture.sh", action],
                   self.work / ("trustee-" + action), timeout=2400, environment=environment)

    def trustee(self, configure=True):
        self.trustee_work = self.work / "trustee"
        self.trustee_action("create")
        info = json.loads((self.trustee_work / "trustee-fixture.json").read_text())
        if configure:
            self.command("trustee-configure", "trustee", "configure", "--url", info["url"],
                         "--admin-key", info["admin_key"])
            doctor = json.loads(self.command("trustee-doctor", "trustee", "doctor", "--json").stdout)
            self.command("trustee-adopt", "trustee", "adopt", "--attestation-policy-sha256", doctor["attestation_policy_sha256"],
                         "--resource-policy-sha256", doctor["resource_policy_sha256"])
        self.monitor_thread = threading.Thread(target=self.monitor_trustee_access, daemon=True)
        self.monitor_thread.start()
        return info

    def monitor_trustee_access(self):
        info = json.loads((self.trustee_work / "trustee-fixture.json").read_text())
        allowed, errors = set(), []
        while not self.monitor_stop.wait(2):
            for target in list(self.resources):
                path = Path(target["state_dir"]) / "services" / target["service"] / "state.json"
                if not path.exists():
                    continue
                try:
                    deployment = json.loads(path.read_text())["deploy"]
                    addresses = deployment.get("instance_public_ips") or [deployment.get("public_ip")]
                    for address in addresses:
                        if not address or address in allowed:
                            continue
                        ipaddress.IPv4Address(address)
                        self.api("ecs", "AuthorizeSecurityGroup", SecurityGroupId=info["security_group_id"],
                                 IpProtocol="tcp", PortRange="8081/8081", SourceCidrIp=address+"/32", Policy="accept")
                        allowed.add(address)
                except Exception as error:
                    errors.append(str(error))
        if errors:
            write_json(self.work / "trustee-access-errors.json", errors)

    def deploy(self, service, spec, timeout=21600, state=None):
        self.register(service, state)
        self.command("deploy-" + service, "deploy", "--spec", spec, timeout=timeout, state=state)
        return self.local_state(service)

    def connect(self, service, label="connect"):
        ready = self.work / (label + "-ready.json")
        self.tunnels.append(ready)
        self.command(label, "connect", "start", "--service", service, "--ready-json", ready,
                     "--wait-ready", "180", timeout=240)
        data = json.loads(ready.read_text())
        return data["client_endpoints"][0]["http_base_url"]

    def ssh(self, service, label, command, instance_id=None, **options):
        arguments = ["ssh", service]
        if instance_id:
            arguments += ["--instance-id", instance_id]
        return self.command(label, *arguments, "--", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                            "-o", "StrictHostKeyChecking=accept-new",
                            "-o", "UserKnownHostsFile=" + str(self.work / "debug-ssh-known-hosts"), command, **options)

    def local_state(self, service):
        return json.loads((self.state / "services" / service / "state.json").read_text())

    def record_resources(self):
        # Capture snapshot associations before destroying imports.
        path = self.work / "cloud-resource-ledger.json"
        # Nested consumer probes may already have destroyed their services.
        for nested in (self.work / "consumer").glob("*/cloud-resource-ledger.json"):
            merge_ledgers(path, read_ledger(nested))
        for resource in self.resources:
            record_service(resource["state_dir"], resource["service"], path)
        run(["python3.11", ROOT / "tools/e2e/cleanup_snapshots.py", "record", path, self.region], self.work / "record-snapshots")

    def cleanup(self):
        errors = []
        for ready in self.tunnels:
            if ready.exists():
                try:
                    self.command("stop-" + ready.stem, "connect", "stop", "--ready-json", ready)
                except RuntimeError as error:
                    errors.append(str(error))
        if self.resources:
            try:
                self.record_resources()
            except Exception as error:
                errors.append(str(error))
        for target in reversed(self.resources):
            if not (Path(target["state_dir"]) / "services" / target["service"] / "state.json").exists():
                continue
            try:
                self.command("destroy-" + target["service"], "destroy", target["service"], state=target["state_dir"], timeout=1800)
            except RuntimeError as error:
                from owned_network_cleanup import cleanup_empty_case_vpc
                try:
                    if cleanup_empty_case_vpc(self.work, target["state_dir"], target["service"], self.api):
                        self.command("destroy-" + target["service"] + "-network-retry", "destroy", target["service"],
                                     state=target["state_dir"], timeout=1800)
                        self.checks["owned_cloud_security_group_cleanup"] = True
                    else:
                        errors.append(str(error))
                except Exception as recovery_error:
                    errors.extend([str(error), str(recovery_error)])
        ledger = self.work / "cloud-resource-ledger.json"
        if ledger.exists():
            try:
                run(["python3.11", ROOT / "tools/e2e/cleanup_snapshots.py", "cleanup", ledger, self.region], self.work / "cleanup-snapshots")
            except RuntimeError as error:
                errors.append(str(error))
        # A retry still needs the Trustee to confirm revocation and finish
        # deleting service resources after a partial provider cleanup failure.
        if self.trustee_work and not errors:
            try:
                self.trustee_action("destroy")
            except RuntimeError as error:
                errors.append(str(error))
        return errors

    def __enter__(self):
        def interrupted(*_):
            raise KeyboardInterrupt("cloud case interrupted")
        for kind in [signal.SIGINT, signal.SIGTERM]:
            self.old_handlers[kind] = signal.signal(kind, interrupted)
        write_json(self.work / "case-result.json", {"passed": False, "cleanup_complete": False})
        return self

    def __exit__(self, kind, error, traceback):
        for number, handler in self.old_handlers.items():
            signal.signal(number, handler)
        self.monitor_stop.set()
        if self.monitor_thread:
            self.monitor_thread.join(timeout=100)
        errors = [] if self.defer_cleanup and error is None else self.cleanup()
        write_json(self.work / "case-result.json", {"schema_version": "confidential-agent/e2e-case/v1",
                   "passed": error is None and not errors, "cleanup_complete": not errors and not self.defer_cleanup,
                   "cleanup_deferred": self.defer_cleanup and error is None,
                   "checks": self.checks, "error": str(error) if error else None, "cleanup_errors": errors})
        if errors and error is None:
            raise RuntimeError("cloud cleanup failed; private diagnostics retained")
