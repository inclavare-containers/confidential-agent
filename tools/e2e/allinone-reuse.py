#!/usr/bin/env python3.11
"""Exercise published-image consumers from private JSON AppSpec fixtures."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
import fcntl
import pty
import re
import secrets
import select
from pathlib import Path
import signal
import shlex
import subprocess
import struct
import termios
import time
import urllib.error
import urllib.request
from resource_ledger import record_service

CRYPTPILOT_GUEST_SHA256 = "2a0d2793b167d18600af103af810fb2bded78a61add99be786a909679d79fece"


def run(command, evidence, timeout=1800, success=True, env=None):
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True, env=env)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        evidence.with_suffix(".out").write_bytes(stdout)
        evidence.with_suffix(".err").write_bytes(stderr)
        if not success:
            return subprocess.CompletedProcess(command, 124, stdout, stderr)
        raise RuntimeError(f"{evidence.name} timed out; see private evidence") from None
    except BaseException:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        raise
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    evidence.with_suffix(".out").write_bytes(result.stdout)
    evidence.with_suffix(".err").write_bytes(result.stderr)
    if success and result.returncode:
        raise RuntimeError(f"{evidence.name} failed; see private evidence")
    return result


class Consumer:
    def __init__(self, fixture, work, ca, cidr, shared_state=None):
        self.spec_path = fixture / "appspec.json"
        self.spec = json.loads(self.spec_path.read_text())
        self.service = self.spec["service"]["id"]
        self.work = work / self.service
        self.work.mkdir(mode=0o700)
        self.state = shared_state or self.work / "state"
        self.ca = [ca, "--state-dir", str(self.state)]
        self.probe = json.loads((fixture / "probe.json").read_text())
        self.cidr = cidr
        self.attempted = False

    def command(self, label, *arguments, **kwargs):
        return run(self.ca + list(arguments), self.work / label, **kwargs)

    def ssh(self, label, *arguments, **kwargs):
        return self.command(label, "ssh", self.service, "--", "-o", "BatchMode=yes",
                            "-o", "ConnectTimeout=10", *arguments, **kwargs)

    @property
    def directory(self):
        return self.state / "services" / self.service

    def deploy(self, concurrent=False, resume=False):
        self.command("validate", "spec", "validate", "--spec", str(self.spec_path))
        peering = self.command("peering-show", "peering", "show", "reuse-test", success=False)
        if peering.returncode:
            self.command("peering", "peering", "add", "--role", "operator",
                         "--cidr", self.cidr, "--label", "reuse-test")
        else:
            assert f"cidr: {self.cidr}".encode() in peering.stdout
        self.attempted = True
        if concurrent:
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda index: self.command(
                    f"concurrent-deploy-{index}", "deploy", "--spec", str(self.spec_path),
                    success=False, timeout=3600), range(2)))
            assert sorted(result.returncode == 0 for result in results) == [False, True]
            rejected = next(result for result in results if result.returncode)
            assert b"destroy it before deploying again" in rejected.stderr, "concurrent rejection was not the service state guard"
        elif resume:
            self.command("deploy-without-delivery", "deploy", "--spec", str(self.spec_path),
                         "--skip-inject", timeout=3600)
            before = json.loads((self.directory / "state.json").read_text())
            assert before["phase"] == "deployed"
            identities = {name: (self.directory / "secrets" / name).read_bytes()
                          for name in ["ssh_client", "ssh_host", "disk_passphrase"]}
            resource = Path(next(iter(self.spec["resources"].values()))["source"])
            original = resource.read_bytes()
            try:
                resource.write_bytes(original + b"changed-resume-input")
                rejected = self.command("reject-changed-resource", "deploy", "--resume", "--spec", str(self.spec_path), success=False)
                assert rejected.returncode and b"deployment input" in rejected.stderr
                moved = self.work / "moved-appspec.json"
                moved.write_bytes(self.spec_path.read_bytes())
                rejected = self.command("reject-moved-resume", "deploy", "--resume", "--spec", str(moved), success=False)
                assert rejected.returncode and b"canonical path" in rejected.stderr
            finally:
                resource.write_bytes(original)
            self.command("resume-deploy", "deploy", "--resume", "--spec", str(self.spec_path),
                         timeout=3600)
            after = json.loads((self.directory / "state.json").read_text())
            assert after["deploy"]["instance_id"] == before["deploy"]["instance_id"]
            assert all((self.directory / "secrets" / name).read_bytes() == value
                       for name, value in identities.items()), "resume replaced an identity"
        else:
            self.command("deploy", "deploy", "--spec", str(self.spec_path), timeout=3600)
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline:
            if self.ssh("wait-ssh", "true", success=False, timeout=20).returncode == 0:
                break
            time.sleep(5)
        else:
            raise RuntimeError(f"{self.service}: authenticated SSH unavailable")
        self.ssh("runtime", "systemctl is-active cai-allinone.service cai-ssh.service")
        self.ssh("single-harness", "! systemctl is-enabled --quiet "
                 "cai-codex-app-server.service cai-hermes-agent.service sshd.service sshd.socket")
        self.ssh("version", "cai-agent --version")
        self.ssh("configuration", "cai-agent --check")
        self.ssh("disk-protection", "set -eu\n"
                 "dmsetup status --target crypt | grep -w crypt\n"
                 "dmsetup status --target verity | grep -w verity")
        self.ssh("private-resource", "test $(stat -c %a /opt/data/allinone/runtime.json) = 600")
        for index, resource in enumerate(self.spec["resources"].values()):
            digest = hashlib.sha256(Path(resource["source"]).read_bytes()).hexdigest()
            result = self.ssh(f"resource-{index}", f"sha256sum {shlex.quote(resource['target'])}")
            assert result.stdout.split()[0].decode() == digest, "delivered resource differs"
        if self.probe.get("runtime_command"):
            deadline = time.monotonic() + 180
            while self.ssh("native-runtime", self.probe["runtime_command"],
                           success=False, timeout=30).returncode:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{self.service}: native runtime unavailable")
                time.sleep(5)
        deadline = time.monotonic() + 180
        while True:
            report = json.loads(self.command("report", "report", "--service", self.service,
                                            "--json").stdout)
            service = next(item for item in report["services"] if item["service_id"] == self.service)
            if (service.get("collect_status") == "ok"
                    and service.get("daemon", {}).get("app_ready")
                    and service["daemon"].get("mesh_ready")):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{self.service}: application and attestation report did not settle")
            time.sleep(5)
        assert report["schema"] == "confidential-agent/attestation-report/v1"
        assert service["collect_status"] == "ok"
        assert service["tee_info"]["status"] == "ok" and service["tee_info"]["tee"] == "tdx"
        assert service["attestation"]["status"] == "ok" and service["attestation"]["ear_jwt"]
        assert service["daemon"]["app_ready"] and service["daemon"]["mesh_ready"]
        manifest = json.loads((self.directory / "manifest.json").read_text())
        assert manifest["image"]["image_id"]
        assert manifest["ssh"]
        assert not list(self.state.rglob("*.qcow2")), "consumer staged a local image"
        if self.probe.get("connect_http_path"):
            self.verify_connect()
        if self.probe.get("command"):
            try:
                result = self.ssh("model", self.probe["command"], timeout=600)
            except RuntimeError:
                self.ssh("model-network-diagnostics", "cat /etc/resolv.conf; "
                         "systemctl show NetworkManager -p ActiveState -p Result; "
                         "journalctl -u NetworkManager --no-pager -n 80",
                         success=False, timeout=30)
                raise
            assert self.probe["expect"].encode() in result.stdout, "model response missing marker"
        else:
            (self.work / "model-skipped.json").write_text(json.dumps({
                "reason": self.probe["skip_reason"]
            }) + "\n")

    def verify_console_isolation(self, aliyun):
        """Verify real console writes; ECS console output is a lifecycle cache."""
        probe = Path(__file__).parent / "probes/console-output.py"
        result = self.ssh("console-kernel-probe", "python3.11 -c " + shlex.quote(probe.read_text()), timeout=90)
        evidence = json.loads(result.stdout)
        assert evidence["passed"]
        cloud = json.loads((self.directory / "state.json").read_text())["deploy"]
        region = self.spec["deploy"]["region"]
        result = run([aliyun, "--region", region, "ecs", "GetInstanceConsoleOutput",
                      "--RegionId", region, "--InstanceId", cloud["instance_id"]],
                     self.work / "console-output")
        console = base64.b64decode(json.loads(result.stdout)["ConsoleOutput"])
        (self.work / "console-output.txt").write_bytes(console)
        for marker in [evidence["stdout_marker"], evidence["stderr_marker"]]:
            assert marker.encode() not in console, "application output reached the cloud console"
        # The API caches startup/restart/shutdown output. Its absence alone is
        # insufficient; the guest probe observes actual writes and proves its
        # tracer with a direct /dev/console write from the same application unit.
        evidence["cloud_console_is_cached"] = True
        (self.work / "console-isolation.json").write_text(json.dumps(evidence) + "\n")

    def verify_management_during_application_errors(self):
        resource = next(value for value in self.spec["resources"].values()
                        if value["target"] == "/opt/data/allinone/runtime.json")
        source = Path(resource["source"])
        original = source.read_bytes()
        cloud = json.loads((self.directory / "state.json").read_text())["deploy"]
        def inject(label):
            self.command(label, "inject", "--spec", str(self.spec_path),
                         "--target-ip", cloud["public_ip"], timeout=600)
        try:
            for label, content in [("invalid-json", b"not-json"), ("invalid-port", json.dumps({"harness": "codex", "mode": "app-server", "port": 4501}).encode())]:
                source.write_bytes(content)
                inject("inject-" + label)
                deadline = time.monotonic() + 90
                while True:
                    checked = self.ssh(label + "-check", "cai-agent --check", success=False)
                    if checked.returncode and b"allinone startup failed" in checked.stderr:
                        break
                    assert time.monotonic() < deadline, "invalid business configuration was not delivered"
                    time.sleep(3)
                self.ssh(label + "-management", "systemctl is-active cai-ssh.service; journalctl -u cai-allinone.service --no-pager >/dev/null")
        finally:
            source.write_bytes(original)
            inject("restore-application")
            self.ssh("restart-restored-application", "systemctl reset-failed cai-allinone.service; systemctl restart cai-allinone.service; cai-agent --check")

    def directory_ports(self, label):
        return json.loads(self.ssh(label, "cat /etc/cai/service-directory.json").stdout)["services"]

    def verify_management_during_token_errors(self):
        original = self.spec_path.read_bytes()
        edited = json.loads(original)
        token = next(resource for resource in edited["resources"].values()
                     if resource["target"].endswith("/.codex/app-server-token"))
        token["mode"] = "0644"
        cloud = json.loads((self.directory / "state.json").read_text())["deploy"]
        def inject(label):
            self.command(label, "inject", "--spec", str(self.spec_path),
                         "--target-ip", cloud["public_ip"], timeout=600)
        try:
            self.spec_path.write_text(json.dumps(edited))
            inject("inject-invalid-token-mode")
            deadline = time.monotonic() + 90
            while True:
                result = self.ssh("token-private-check", "cai-agent --check", success=False)
                if result.returncode and b"private capability token" in result.stderr:
                    break
                assert time.monotonic() < deadline, "invalid token permissions were not applied"
                time.sleep(3)
            self.ssh("token-error-management", "systemctl is-active cai-ssh.service; journalctl -u cai-allinone.service --no-pager >/dev/null")
        finally:
            self.spec_path.write_bytes(original)
            inject("restore-token-mode")
            self.ssh("restart-server-after-token-error", "systemctl reset-failed cai-allinone.service; systemctl restart cai-allinone.service; cai-agent --check")

    def verify_software_versions(self, expected):
        script = """import json, pathlib, subprocess
root = pathlib.Path(subprocess.check_output(['npm','root','-g'],text=True).strip())
packages = {'openclaw':'openclaw','codex':'@openai/codex','claude-code':'@anthropic-ai/claude-code','qodercli':'@qoder-ai/qodercli','opencode':'opencode-ai'}
actual = {name: json.loads((root / package / 'package.json').read_text())['version'] for name,package in packages.items()}
actual['hermes'] = subprocess.check_output(['git','-C','/usr/local/lib/hermes-agent','rev-parse','HEAD'],text=True).strip()
actual['node'] = subprocess.check_output(['node','--version'],text=True).strip().removeprefix('v')
print(json.dumps(actual))
"""
        actual = json.loads(self.ssh("actual-software-versions", "python3.11 -c " + shlex.quote(script)).stdout)
        for name, version in actual.items():
            assert actual[name] == expected[name], f"{self.service}: {name} version differs from the pinned version"
        return actual

    def verify_pty(self):
        """Run a real interactive remote terminal, with optional native CLI exchanges."""
        script = "set -e; test -t 0 && test -t 1; stty size; printf PTY_READY; read value; test \"$value\" = PTY_INPUT; printf PTY_ACCEPTED; stty size; exit 7"
        self.interactive("pty-shell", script, [{"expect": "PTY_READY", "send": "PTY_INPUT\n"},
                                             {"expect": "PTY_ACCEPTED"}, {"expect": "30 100"}], exit_code=7)
        runtime = json.loads(self.ssh("pty-runtime", "cai-agent --check").stdout)
        if runtime["harness"] == "codex":
            probe = Path(__file__).parent / "probes/codex-skills.py"
            self.ssh("codex-tdx-skill", "python3.11 -c " + shlex.quote(probe.read_text()), timeout=90)
        else:
            probe = Path(__file__).parent / "probes/harness-skills.py"
            self.ssh("native-tdx-skill", "python3.11 -c " + shlex.quote(probe.read_text()), timeout=90)
        if runtime["harness"] == "codex" and runtime["mode"] == "cli":
            # `exec` and explicit app-server bypass the default daemon startup,
            # so they cannot detect missing process-management tools such as ps.
            command = '''set -e; work=$(mktemp -d /tmp/codex-startup.XXXXXX); trap 'rm -rf "$work"' EXIT
cai-agent --no-alt-screen --sandbox read-only -C "$work" -c "projects.\\"$work\\".trust_level=\\"trusted\\""'''
            self.interactive("codex-default-startup", command,
                             [{"expect": "OpenAI Codex", "send": "/exit\r", "key_delay": 0.1}],
                             exit_code=0)
        if self.probe.get("interactive"):
            self.interactive("pty-harness", self.probe["interactive"]["command"],
                             self.probe["interactive"]["exchanges"],
                             exit_code=self.probe["interactive"].get("exit_code"))

    def interactive(self, label, command, exchanges, exit_code=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        env = dict(os.environ, TERM="xterm-256color")
        process = subprocess.Popen(self.ca + ["ssh", self.service, "--tty", "--", command],
                                   stdin=slave, stdout=slave, stderr=slave, env=env,
                                   start_new_session=True)
        os.close(slave)
        transcript = bytearray()
        cursor = 0
        try:
            for exchange in exchanges:
                deadline = time.monotonic() + exchange.get("timeout", 120)
                expected = exchange["expect"].encode()
                while expected not in transcript[cursor:]:
                    if select.select([master], [], [], 1)[0]:
                        try:
                            chunk = os.read(master, 65536)
                        except OSError:
                            chunk = b""
                        transcript.extend(chunk)
                        # Answer the terminal cursor query used by native TUIs.
                        if b"\x1b[6n" in chunk:
                            os.write(master, b"\x1b[1;1R")
                    if expected not in transcript[cursor:] and (time.monotonic() >= deadline or process.poll() is not None):
                        raise RuntimeError(f"{label}: interactive expectation failed; see private transcript")
                cursor = transcript.index(expected, cursor) + len(expected)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
                if process.poll() is None:
                    try:
                        os.kill(process.pid, signal.SIGWINCH)
                    except ProcessLookupError:
                        pass
                if "send" in exchange:
                    # Native TUIs can interpret a burst of text plus Enter as
                    # pasted multiline input. Allow fixtures to type real keys.
                    key_delay = exchange.get("key_delay", 0)
                    if key_delay:
                        for character in exchange["send"]:
                            os.write(master, character.encode())
                            time.sleep(key_delay)
                    else:
                        os.write(master, exchange["send"].encode())
            if exit_code is not None:
                assert process.wait(timeout=30) == exit_code, "interactive exit code was not preserved"
        finally:
            (self.work / (label + ".transcript")).write_bytes(transcript)
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            os.close(master)

    def verify_connect(self):
        ready = self.work / "connect-ready.json"
        try:
            self.command("connect-start", "connect", "start", "--service", self.service,
                         "--ready-json", str(ready), "--wait-ready", "180",
                         "--log-file", str(self.work / "connect.log"), timeout=600)
            endpoints = json.loads(ready.read_text())["client_endpoints"]
            assert len(endpoints) == 1
            endpoint = endpoints[0]
            assert endpoint["local_host"] == "127.0.0.1"
            url = endpoint["http_base_url"] + self.probe["connect_http_path"]
            deadline = time.monotonic() + 180
            while True:
                try:
                    with urllib.request.urlopen(url, timeout=15) as response:
                        assert response.status == 200
                        (self.work / "connect-health.json").write_bytes(response.read())
                    break
                except (urllib.error.URLError, TimeoutError, ConnectionError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{self.service}: connected HTTP health unavailable") from None
                    time.sleep(5)
        except Exception:
            if ready.exists():
                log = Path(json.loads(ready.read_text())["log_file"])
                if log.exists():
                    (self.work / "connect-diagnostics").write_bytes(log.read_bytes()[-65536:])
            raise
        finally:
            if ready.exists():
                self.command("connect-stop", "connect", "stop", "--ready-json", str(ready))

    def destroy(self):
        if not self.attempted:
            return
        record_service(self.state, self.service, self.work / "cloud-resource-ledger.json")
        for attempt in range(3):
            result = self.command(f"destroy-{attempt}", "destroy", self.service,
                                  success=False, timeout=1800)
            if result.returncode == 0:
                return
        raise RuntimeError(f"{self.service}: cleanup failed; see private state")


def reject_invalid_release(consumer):
    spec = consumer.spec
    altered_release = consumer.work / "altered-release.json"
    envelope = json.loads(Path(spec["deploy"]["image"]["release"]).read_text())
    release = json.loads(base64.b64decode(envelope["payload"]))
    mapping = next(value for value in release["images"] if value["region"] == spec["deploy"]["region"])
    # Old specs can retain an account field, but image authorization comes from
    # the authenticated release mapping. Rendering does not allocate resources.
    legacy = json.loads(json.dumps(spec))
    legacy["service"]["id"] += "-legacy-account"
    legacy["deploy"]["image"]["account_id"] = "0"
    legacy_path = consumer.spec_path.parent / "legacy-account.json"
    legacy_path.write_text(json.dumps(legacy))
    consumer.command("accept-legacy-account", "deploy", "--spec", str(legacy_path), "--render-only")
    legacy_directory = consumer.state / "services" / legacy["service"]["id"]
    rendered = json.loads((legacy_directory / "manifest.json").read_text())
    selected_image = spec["deploy"]["image"].get("image_id", mapping["image_id"])
    assert rendered["image"]["image_id"] == selected_image, "legacy account changed the authenticated image selection"
    assert not (legacy_directory / "state.json").exists(), "render-only recorded an infrastructure deployment"
    altered_mapping = consumer.work / "altered-mapping-release.json"
    mapping_envelope = json.loads(json.dumps(envelope))
    mapping["image_id"] += "-mismatch"
    mapping_envelope["payload"] = base64.b64encode(json.dumps(release).encode()).decode()
    altered_mapping.write_text(json.dumps(mapping_envelope))
    signature = envelope["signatures"][0]["sig"]
    envelope["signatures"][0]["sig"] = ("A" if signature[0] != "A" else "B") + signature[1:]
    altered_release.write_text(json.dumps(envelope))
    wrong_key = consumer.work / "wrong-publisher.key"
    wrong_public = consumer.work / "wrong-publisher.pem"
    run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(wrong_key)],
        consumer.work / "wrong-publisher-key")
    run(["openssl", "pkey", "-in", str(wrong_key), "-pubout", "-out", str(wrong_public)],
        consumer.work / "wrong-publisher-public")
    # image_id in an AppSpec is an explicit image override. The mapping negative
    # instead alters publisher-authenticated payload bytes without resigning.
    for label, field, wrong in [("manifest", "manifest", "sha256:" + "0" * 64),
                                ("image_id", "release", str(altered_mapping)),
                                ("release", "release", str(altered_release)),
                                ("trusted_key", "trusted_key", str(wrong_public))]:
        invalid = json.loads(json.dumps(spec))
        invalid["service"]["id"] += "-invalid-" + label.replace("_", "-")
        invalid["deploy"]["image"][field] = wrong
        path = consumer.spec_path.parent / f"invalid-{label}.json"
        path.write_text(json.dumps(invalid))
        result = consumer.command(f"reject-{label}", "deploy", "--spec", str(path),
                                  "--render-only", success=False)
        assert result.returncode != 0, f"accepted invalid {label}"
        assert b"authentication failed" in result.stderr, "failed outside release verification"
        rejected = consumer.state / "services" / invalid["service"]["id"]
        assert not (rejected / "state.json").exists()
        assert not (rejected / "bootstrap.json").exists()


def reject_incompatible_release(consumer, release):
    spec = json.loads(json.dumps(consumer.spec))
    spec["service"]["id"] += "-incompatible"
    spec["deploy"]["image"]["release"] = str(release.resolve())
    path = consumer.spec_path.parent / "incompatible-appspec.json"
    path.write_text(json.dumps(spec))
    result = consumer.command("reject-incompatible-runtime", "deploy", "--spec", str(path),
                              "--render-only", success=False)
    assert result.returncode and b"runtime contract" in result.stderr
    assert b"authentication failed" not in result.stderr, "incompatible fixture did not have a valid publisher signature"
    directory = consumer.state / "services" / spec["service"]["id"]
    assert not (directory / "state.json").exists()
    assert not (directory / "bootstrap.json").exists()


def assert_independent(consumers):
    for name in ["ssh_client", "ssh_host", "disk_passphrase"]:
        values = [(item.directory / "secrets" / name).read_bytes() for item in consumers]
        assert len(set(values)) == len(values), f"consumers share {name}"
    identities = [json.loads((item.directory / "state.json").read_text())
                  ["gateway_identity"]["public_key"] for item in consumers]
    assert len(set(identities)) == len(identities), "consumers share a gateway identity"
    first, second = consumers[:2]
    state = json.loads((second.directory / "state.json").read_text())
    host = state["deploy"]["public_ip"]
    manifest = json.loads((second.directory / "manifest.json").read_text())
    identity = manifest["ssh"]
    wrong_client = run([
        "ssh", "-i", str(first.directory / "secrets/ssh_client"),
        "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
        "-o", "StrictHostKeyChecking=yes", "-o", f"HostKeyAlias={identity['host_alias']}",
        "-o", f"UserKnownHostsFile={identity['known_hosts']}", f"root@{host}", "true"
    ], second.work / "reject-other-client", success=False, timeout=30)
    assert wrong_client.returncode != 0 and b"Permission denied" in wrong_client.stderr
    wrong_hosts = second.work / "wrong-known-hosts"
    wrong_hosts.write_text(identity["host_alias"] + " " +
                           (first.directory / "secrets/ssh_host.pub").read_text())
    wrong_host = run([
        "ssh", "-i", identity["private_key"], "-o", "IdentitiesOnly=yes",
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=yes",
        "-o", f"HostKeyAlias={identity['host_alias']}",
        "-o", f"UserKnownHostsFile={wrong_hosts}", f"root@{host}", "true"
    ], second.work / "reject-other-host", success=False, timeout=30)
    assert wrong_host.returncode != 0 and b"Host key verification failed" in wrong_host.stderr


def owned_cloud_instances(consumer, aliyun, label):
    image = json.loads((consumer.directory / "manifest.json").read_text())["image"]["image_id"]
    region = consumer.spec["deploy"]["region"]
    matches = []
    page = 1
    while True:
        result = json.loads(run([aliyun, "--region", region, "ecs", "DescribeInstances",
                                 "--RegionId", region, "--ImageId", image, "--PageSize", "100",
                                 "--PageNumber", str(page)],
                                consumer.work / f"{label}-{page}").stdout)
        matches.extend(item["InstanceId"] for item in result["Instances"]["Instance"]
                       if any(tag["TagKey"] == "confidential-agent-service" and tag["TagValue"] == consumer.service
                              for tag in item.get("Tags", {}).get("Tag", [])))
        if page * 100 >= result["TotalCount"]:
            break
        page += 1
    return matches


def assert_one_deployment(consumer, aliyun):
    state = json.loads((consumer.directory / "state.json").read_text())
    assert owned_cloud_instances(consumer, aliyun, "concurrent-instances") == [state["deploy"]["instance_id"]], "same-service deployment created unowned instances"


def write_interruption_wrapper(wrapper, real, marker):
    wrapper.write_text("#!/bin/sh\n" + shlex.quote(real) + ' "$@"\nstatus=$?\n'
                       + '[ "$status" = 0 ] || exit "$status"\n'
                       + 'for arg in "$@"; do\ncase "$arg" in --release)\n'
                       + "touch " + shlex.quote(str(marker)) + "\nexec sleep 3600\n;; esac\ndone\n")
    wrapper.chmod(0o700)


def interrupt_infrastructure(consumer, aliyun):
    """Kill CA after a real provider apply, before CA can confirm its outcome."""
    marker = consumer.work / "provider-completed"
    wrapper = consumer.work / "shelter-interruption"
    real = os.environ.get("CA_SHELTER_BIN") or subprocess.check_output(
        ["sh", "-c", "command -v shelter"], text=True).strip()
    write_interruption_wrapper(wrapper, real, marker)
    consumer.attempted = True
    process = None
    try:
        with (consumer.work / "interrupted-deploy.out").open("wb") as stdout, (consumer.work / "interrupted-deploy.err").open("wb") as stderr:
            process = subprocess.Popen(consumer.ca + ["--shelter-bin", str(wrapper), "deploy", "--spec", str(consumer.spec_path)],
                                       stdout=stdout, stderr=stderr, start_new_session=True)
            deadline = time.monotonic() + 1800
            while not marker.exists():
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("interruption fixture did not complete its real provider apply")
                time.sleep(2)
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        state = json.loads((consumer.directory / "state.json").read_text())
        assert state["phase"] == "deploying", "CA had already confirmed the provider result"
        rejected = consumer.command("reject-unknown-resume", "deploy", "--resume", "--spec", str(consumer.spec_path), success=False)
        assert rejected.returncode and b"interrupted infrastructure operation" in rejected.stderr
        consumer.destroy()
        consumer.attempted = False
        assert not owned_cloud_instances(consumer, aliyun, "interrupted-instance-deleted"), "interrupted deployment leaked an instance"
    finally:
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def capture_boot_console(consumer, aliyun, label, boot_id):
    raw_state = (consumer.directory / "state.json").read_bytes()
    cloud = json.loads(raw_state)["deploy"]
    region = consumer.spec["deploy"]["region"]
    result = run([aliyun, "--region", region, "ecs", "GetInstanceConsoleOutput", "--RegionId", region,
                  "--InstanceId", cloud["instance_id"], "--RemoveSymbols", "false"], consumer.work / label)
    response = json.loads(result.stdout)
    assert response["InstanceId"] == cloud["instance_id"], "console response instance differs from deployment"
    console = base64.b64decode(response["ConsoleOutput"], validate=True)
    assert console, "original boot console is unavailable"
    (consumer.work / (label + ".txt")).write_bytes(console)
    (consumer.work / (label + "-state.private.json")).write_bytes(raw_state)
    receipt = {"instance_id": cloud["instance_id"], "observed_guest_boot_id": boot_id.decode(),
               "provider_last_update_time": response.get("LastUpdateTime"), "captured_at": time.time(),
               "state_sha256": hashlib.sha256(raw_state).hexdigest(),
               "response_sha256": hashlib.sha256(result.stdout).hexdigest(),
               "console_sha256": hashlib.sha256(console).hexdigest(), "console_bytes": len(console),
               "attestation_or_admission_claimed": False}
    (consumer.work / (label + "-receipt.json")).write_text(json.dumps(receipt, indent=2) + "\n")
    return console


def verify_boot_storage(evidence, console, journal, require_resize):
    assert not evidence.get("error"), "filesystem evidence collection failed"
    assert all(command["exit_code"] == 0 for command in evidence["commands"].values()), "filesystem command failed"
    assert evidence["commands"]["crypt"]["stdout"].strip(), "encrypted mapping is missing"
    assert evidence["commands"]["verity"]["stdout"].strip(), "verified mapping is missing"
    def devices(rows):
        for row in rows:
            yield row
            yield from devices(row.get("children", []))
    for label in ["root", "data"]:
        filesystem = evidence["mounts"][label]
        assert filesystem["mount"]["fstype"] == "ext4", label + " filesystem is not ext4"
        assert any(row["type"] == "crypt" for row in devices(filesystem["device_chain"])), label + " has no encrypted device ancestor"
        blocks, block_bytes = filesystem["filesystem_blocks"], filesystem["filesystem_block_bytes"]
        assert blocks > 0 and block_bytes > 0 and filesystem["statvfs_bytes"] > 0
        assert 0 <= filesystem["device_bytes"] - blocks * block_bytes < block_bytes, label + " filesystem does not fill its device"
        assert "error" not in filesystem["filesystem_state"].lower(), label + " filesystem records errors"
    assert evidence["mounts"]["root"]["cryptpilot_root_device"], "mounted root is not Cryptpilot's rootfs device"
    logs = (console + b"\n" + journal).decode(errors="replace")
    fatal = re.compile(r"AEAD[^\n]*(?:fail|error)|Buffer I/O error|EXT4-fs[^\n]*(?:\berrors?\b(?!\s*=)|corrupt)|"
                       r"filesystem[^\n]*(?:uncorrected|corrupt)|UNEXPECTED INCONSISTENCY|"
                       r"I/O error|Kernel panic|Failed to (?:check|resize) ext4 filesystem|"
                       r"Filesystem check failed with exit code|(?:fail|error)[^\n]*mount[^\n]*(?:sysroot|rootfs)", re.I)
    assert not fatal.search(logs), "boot contains a filesystem, integrity or mount failure"
    if require_resize:
        # The pinned official guest logs success only after accepted e2fsck
        # (exit 0 or 1) and resize2fs. This does not claim the repair branch ran.
        original = console.decode(errors="replace").splitlines()
        start = next((index for index, line in enumerate(original)
                      if "Resizing ext4 filesystem to fill device" in line and "/dev/mapper/rootfs" in line), None)
        assert start is not None, "first boot console lacks the actual rootfs resize operation"
        assert any("ext4 filesystem resized successfully" in line for line in original[start + 1:]), \
            "first boot console lacks subsequent successful fsck/resize completion"


def cryptpilot_boot_evidence(consumer, label, boot_id, console, require_resize=False):
    digest = consumer.ssh(label + "-cryptpilot-sha256", "sha256sum /usr/bin/cryptpilot-fde").stdout.split()[0].decode()
    assert digest == CRYPTPILOT_GUEST_SHA256, "actual guest Cryptpilot differs from the official pinned ELF"
    version = consumer.ssh(label + "-cryptpilot-version", "/usr/bin/cryptpilot-fde --version").stdout.decode()
    assert version.splitlines() and version.splitlines()[0] == "cryptpilot-fde 0.9.3", "actual guest Cryptpilot version differs"
    probe = Path(__file__).parent / "probes/cryptpilot-filesystems.py"
    filesystem = consumer.ssh(label + "-filesystems", "python3.11 -c " + shlex.quote(probe.read_text()))
    evidence = json.loads(filesystem.stdout)
    assert evidence["boot_id"].encode() == boot_id, "filesystem probe ran in a different boot"
    journal = consumer.ssh(label + "-journal", "journalctl -b --no-pager -o short-monotonic").stdout
    verify_boot_storage(evidence, console, journal, require_resize)
    (consumer.work / (label + "-cryptpilot-proof.json")).write_text(json.dumps({
        "guest_sha256": digest, "guest_version": version, "boot_id": boot_id.decode(),
        "filesystem_probe_sha256": hashlib.sha256(probe.read_bytes()).hexdigest(),
        "filesystem_evidence_sha256": hashlib.sha256(filesystem.stdout).hexdigest(),
        "journal_sha256": hashlib.sha256(journal).hexdigest(),
        "first_boot_fsck_and_resize_completed": require_resize,
        "fsck_repair_exit_one_claimed": False}, indent=2) + "\n")


def reboot(consumer, aliyun):
    before = consumer.ssh("boot-before", "cat /proc/sys/kernel/random/boot_id").stdout.strip()
    assert before, "guest boot ID is empty"
    console = capture_boot_console(consumer, aliyun, "first-boot-console", before)
    cryptpilot_boot_evidence(consumer, "first-boot", before, console, require_resize=True)
    endpoints = consumer.directory_ports("endpoints-before-reboot")
    marker = secrets.token_hex(32)
    consumer.ssh("persistent-write", "printf '%s\\n' " + shlex.quote(marker) + " > /opt/data/allinone/reboot-marker; sync")
    state = json.loads((consumer.directory / "state.json").read_text())
    cloud = state["deploy"]
    region = consumer.spec["deploy"]["region"]
    run([aliyun, "--region", region, "ecs", "RebootInstance", "--RegionId", region,
         "--InstanceId", cloud["instance_id"], "--ForceStop", "true"],
        consumer.work / "force-reboot")
    time.sleep(30)
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        if consumer.spec["attestation"]["mode"] == "challenge":
            consumer.command("reinject", "inject", "--spec", str(consumer.spec_path),
                             "--target-ip", cloud["public_ip"], success=False, timeout=60)
        result = consumer.ssh("boot-after", "cat /proc/sys/kernel/random/boot_id",
                              success=False, timeout=20)
        if result.returncode == 0 and result.stdout.strip() != before:
            after = result.stdout.strip()
            assert after, "guest boot ID is empty after reboot"
            consumer.ssh("persistent-after", "grep -Fx " + shlex.quote(marker) + " /opt/data/allinone/reboot-marker")
            consumer.ssh("runtime-after", "systemctl is-active cai-allinone.service cai-ssh.service")
            console = capture_boot_console(consumer, aliyun, "reboot-console", after)
            cryptpilot_boot_evidence(consumer, "reboot", after, console)
            deadline = time.monotonic() + 90
            while consumer.directory_ports("endpoints-after-reboot") != endpoints:
                assert time.monotonic() < deadline, "reboot changed peer endpoint assignments"
                time.sleep(3)
            (consumer.work / "encrypted-reboot-proof.json").write_text(json.dumps({
                "boot_before": before.decode(), "boot_after": after.decode(), "force_stop": True,
                "persistent_marker_sha256": hashlib.sha256((marker + "\n").encode()).hexdigest(),
                "endpoints_unchanged": True, "official_guest_checked_before_and_after": True}, indent=2) + "\n")
            return
        time.sleep(10)
    raise RuntimeError(f"{consumer.service}: reboot recovery failed")


def trustee_outage(consumer, aliyun, unit=None, control=None):
    """The owned disposable Trustee must gate cold-boot key release."""
    before = consumer.ssh("outage-boot-before", "cat /proc/sys/kernel/random/boot_id").stdout.strip()
    cloud = json.loads((consumer.directory / "state.json").read_text())["deploy"]
    region = consumer.spec["deploy"]["region"]
    run([control, "stop"] if control else ["systemctl", "stop", unit], consumer.work / "trustee-stop")
    try:
        run([aliyun, "--region", region, "ecs", "RebootInstance", "--RegionId", region,
             "--InstanceId", cloud["instance_id"], "--ForceStop", "true"],
            consumer.work / "outage-reboot")
        time.sleep(30)
        for attempt in range(8):
            assert consumer.ssh(f"outage-ssh-{attempt}", "true", success=False,
                                timeout=20).returncode != 0, "boot bypassed unavailable Trustee"
            time.sleep(15)
        output = run([aliyun, "--region", region, "ecs", "GetInstanceConsoleOutput", "--RegionId", region,
                      "--InstanceId", cloud["instance_id"]], consumer.work / "outage-console")
        console = base64.b64decode(json.loads(output.stdout)["ConsoleOutput"]).decode(errors="replace")
        (consumer.work / "outage-console.txt").write_text(console)
        assert "trustee" in console.lower() and "failed" in console.lower(), "missing key-release failure evidence"
        instances = json.loads(run([
            aliyun, "--region", region, "ecs", "DescribeInstances", "--RegionId", region,
            "--InstanceIds", json.dumps([cloud["instance_id"]])
        ], consumer.work / "outage-before-stop").stdout)["Instances"]["Instance"]
        assert len(instances) == 1
        if instances[0]["Status"] == "Running":
            run([aliyun, "--region", region, "ecs", "StopInstance", "--RegionId", region,
                 "--InstanceId", cloud["instance_id"], "--ForceStop", "true",
                 "--StoppedMode", "KeepCharging"], consumer.work / "outage-force-stop")
    finally:
        run([control, "start"] if control else ["systemctl", "start", unit], consumer.work / "trustee-start")
    deadline = time.monotonic() + 600
    attempt = 0
    while time.monotonic() < deadline:
        # The guest powers off when key release fails. ECS can finish that
        # transition after Trustee returns, so observe it throughout recovery.
        instances = json.loads(run([
            aliyun, "--region", region, "ecs", "DescribeInstances", "--RegionId", region,
            "--InstanceIds", json.dumps([cloud["instance_id"]])
        ], consumer.work / f"outage-instance-status-{attempt}").stdout)["Instances"]["Instance"]
        assert len(instances) == 1
        if instances[0]["Status"] == "Stopped":
            run([aliyun, "--region", region, "ecs", "StartInstance", "--RegionId", region,
                 "--InstanceId", cloud["instance_id"]],
                consumer.work / f"outage-start-instance-{attempt}")
        attempt += 1
        result = consumer.ssh("outage-boot-after", "cat /proc/sys/kernel/random/boot_id",
                              success=False, timeout=20)
        if result.returncode == 0 and result.stdout.strip() != before:
            consumer.ssh("outage-runtime-after", "systemctl is-active cai-allinone.service cai-ssh.service")
            return
        time.sleep(10)
    raise RuntimeError(f"{consumer.service}: Trustee recovery failed without reinjection")


def deploy_with_frozen_inputs(consumers, parallel, aliyun):
    """Changing or removing A's source must not apply its edits when B is added."""
    first, second, *remaining = consumers
    assert remaining, "frozen-input coverage needs three consumers"
    original = first.spec_path.read_bytes()
    saved = first.spec_path.with_suffix(".saved")
    before = json.loads((first.directory / "manifest.json").read_text())["deployment"]
    cloud = json.loads((first.directory / "state.json").read_text())["deploy"]
    region = first.spec["deploy"]["region"]
    def cloud_configuration(label):
        instance = json.loads(run([aliyun, "--region", region, "ecs", "DescribeInstances",
                                  "--RegionId", region, "--InstanceIds", json.dumps([cloud["instance_id"]])],
                                 first.work / (label + "-instance")).stdout)["Instances"]["Instance"]
        disks = json.loads(run([aliyun, "--region", region, "ecs", "DescribeDisks", "--RegionId", region,
                               "--InstanceId", cloud["instance_id"]],
                              first.work / (label + "-disks")).stdout)["Disks"]["Disk"]
        assert len(instance) == 1
        return instance[0]["InstanceType"], sorted((item["DiskId"], item["Size"]) for item in disks)
    before_cloud = cloud_configuration("frozen-before")
    edited = json.loads(original)
    edited["deploy"]["instance_type"] = "ecs.g9i.2xlarge"
    edited["deploy"]["disk_gb"] += 40
    edited["service"]["ports"] = [19999]
    first.spec_path.write_text(json.dumps(edited))
    try:
        second.deploy()
        first.spec_path.rename(saved)
        with ThreadPoolExecutor(max_workers=parallel) as pool:
            list(pool.map(lambda item: item.deploy(), remaining))
        after = json.loads((first.directory / "manifest.json").read_text())["deployment"]
        assert after == before, "background maintenance replaced committed deployment inputs"
        current = json.loads((first.directory / "state.json").read_text())["deploy"]
        assert current == cloud, "background maintenance changed instance ownership"
        assert cloud_configuration("frozen-after") == before_cloud, "background maintenance changed cloud compute/storage"
        first.ssh("unchanged-runtime", "cai-agent --check")
    finally:
        first.spec_path.write_bytes(original)
        saved.unlink(missing_ok=True)


def stable_endpoint_peers(consumers, extra_spec):
    ports = extra_spec["service"]["ports"]
    assert ports and not set(ports).intersection(extra_spec["service"].get("connect", [])), \
        "stable endpoint coverage requires mesh ports, not operator-only connect ports"
    peers = [item for item in consumers if item.spec["service"]["ports"] == ports
             and not set(ports).intersection(item.spec["service"].get("connect", []))]
    assert len(peers) >= 2, "stable endpoint coverage needs two existing mesh peers on the extra peer's port"
    assert extra_spec["service"]["id"] < min(item.service for item in peers), \
        "extra peer must sort before existing peers"
    return peers


def verify_stable_endpoints(consumers, fixture):
    client = next(item for item in consumers if not item.spec["service"]["ports"])
    extra = Consumer(fixture.resolve(), client.work.parent, client.ca[0], client.cidr, client.state)
    peers = stable_endpoint_peers(consumers, extra.spec)
    before = client.directory_ports("stable-endpoints-before")
    assert all(item.service in before for item in peers)
    changed = []
    dropin = "/run/systemd/system/cai-allinone.service.d/routing-test.conf"
    probe = "/run/cai-e2e-routing.py"

    def install_identity_server(item):
        port, = item.spec["service"]["ports"]
        script = ("from http.server import BaseHTTPRequestHandler, HTTPServer\n"
                  "class Handler(BaseHTTPRequestHandler):\n"
                  " def do_GET(self):\n"
                  "  self.send_response(200); self.end_headers(); self.wfile.write(" + repr(item.service.encode()) + ")\n"
                  " def log_message(self, *args): pass\n"
                  "HTTPServer(('0.0.0.0', " + str(port) + "), Handler).serve_forever()\n")
        unit = "[Service]\nExecStart=\nExecStart=/usr/bin/python3.11 " + probe + "\n"
        changed.append(item)
        item.ssh("routing-probe-install", "mkdir -p " + shlex.quote(str(Path(dropin).parent))
                 + "; printf %s " + shlex.quote(script) + " > " + probe
                 + "; printf %s " + shlex.quote(unit) + " > " + dropin
                 + "; systemctl daemon-reload; systemctl restart cai-allinone.service")

    def verify_routes(label, targets, absent=()):
        deadline = time.monotonic() + 180
        while True:
            directory = client.directory_ports(label + "-directory")
            if all(item.service in directory for item in targets) and all(id not in directory for id in absent):
                break
            assert time.monotonic() < deadline, "mesh directory did not converge"
            time.sleep(3)
        for item in peers:
            assert directory[item.service] == before[item.service], "existing endpoint changed target"
        for item in targets:
            port = directory[item.service]["ports"][0]["port"]
            command = ("python3.11 -c " + shlex.quote("import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:"
                       + str(port) + "/', timeout=10).read().decode())"))
            while True:
                result = client.ssh(label + "-call-" + item.service, command, success=False, timeout=20)
                if result.returncode == 0 and result.stdout.strip().decode() == item.service:
                    break
                assert time.monotonic() < deadline, "stable endpoint did not reach its attested service"
                time.sleep(3)
        return directory

    try:
        for item in peers:
            install_identity_server(item)
        verify_routes("stable-before", peers)
        extra.deploy()
        install_identity_server(extra)
        after = verify_routes("stable-added", peers + [extra])
        removed_port = after[extra.service]["ports"][0]["port"]
        removed_id = extra.service
        extra.destroy()
        extra.attempted = False
        verify_routes("stable-removed", peers, [removed_id])
        replacement = client.work.parent / "replacement-fixture"
        replacement.mkdir(mode=0o700)
        spec = json.loads(json.dumps(extra.spec))
        spec["service"]["id"] = "zzzz-" + extra.service
        (replacement / "appspec.json").write_text(json.dumps(spec))
        (replacement / "probe.json").write_text(json.dumps(extra.probe))
        extra = Consumer(replacement, client.work.parent, client.ca[0], client.cidr, client.state)
        extra.deploy()
        install_identity_server(extra)
        after = verify_routes("stable-replaced", peers + [extra], [removed_id])
        assert after[extra.service]["ports"][0]["port"] != removed_port, "removed address was recycled for another service"
    finally:
        for item in changed:
            if item is extra or item in peers:
                item.ssh("routing-probe-remove", "rm -f " + dropin + " " + probe
                         + "; systemctl daemon-reload; systemctl restart cai-allinone.service", success=False)
        extra.destroy()


def verify_publication_cleanup(consumers, publication_state, aliyun):
    consumer = consumers[0]
    image = json.loads((consumer.directory / "manifest.json").read_text())["image"]["image_id"]
    publisher = json.loads(publication_state.read_text())
    entries = {key: value for key, value in publisher["build"]["published"].items()
               if value.get("image_id") == image}
    assert len(entries) == 1, "publication evidence does not identify the selected image"
    entry, = entries.values()
    publication_key, = entries
    assert entry["status"] == "available" and not entry.get("retiring", False)
    # Use a local publication fixture to isolate reference protection from the
    # export-retention flag. Dummy credentials keep every delete unauthorized.
    entry.update(exported=False, oss_cleaned=True)
    # Public exports omit private snapshot custody; serde defaults it to [].
    entry.setdefault("import_snapshot_ids", [])
    publisher.update(service_id="reuse-test-publisher", phase="built")
    publisher["build"]["published"] = entries
    publisher["deploy"]["published_image_id"] = None
    directory = consumer.state / "services/reuse-test-publisher"
    directory.mkdir(mode=0o700)
    state_file = directory / "state.json"
    state_file.write_text(json.dumps(publisher))
    denied_env = dict(os.environ)
    for key in ["ALICLOUD_ACCESS_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID"]:
        denied_env[key] = "CA_E2E_INVALID_ACCESS_KEY"
    for key in ["ALICLOUD_SECRET_KEY", "ALIBABA_CLOUD_ACCESS_KEY_SECRET"]:
        denied_env[key] = "CA_E2E_INVALID_SECRET_KEY"
    for key in ["ALICLOUD_STS_TOKEN", "ALIBABA_CLOUD_SECURITY_TOKEN"]:
        denied_env.pop(key, None)
    base = ["image", "unpublish", "reuse-test-publisher", "--image-id", image]
    rejected = consumer.command("cleanup-reject-consumers", *base, success=False, env=denied_env)
    assert rejected.returncode and b"is retained" in rejected.stderr
    assert any(item.service.encode() in rejected.stderr for item in consumers), "cleanup did not inspect independent consumers"
    preview = consumer.command("cleanup-prune-retains-consumers", "image", "prune", "--dry-run", "--all")
    assert image.encode() not in b"\n".join(line for line in preview.stdout.splitlines() if b"would prune service=reuse-test-publisher" in line)
    failed = consumer.command("cleanup-denied-cloud-delete", *base, "--force", success=False, env=denied_env)
    assert failed.returncode and b"partially failed" in failed.stderr
    written = json.loads(state_file.read_text())["build"]["published"]
    # Retirement intent is persisted independently from publication availability.
    # Denied credentials must retain the same image and cleanup custody for retry.
    retained = written.get(publication_key)
    assert set(written) == set(entries) and retained is not None
    assert retained.get("retiring") is True
    for field in ["status", "image_id", "owned", "oss_cleaned", "import_task_id", "import_snapshot_ids"]:
        assert retained.get(field) == entry.get(field), "denied retirement changed " + field
    region = consumer.spec["deploy"]["region"]
    result = run([aliyun, "--region", region, "ecs", "DescribeImages", "--RegionId", region, "--ImageId", image], consumer.work / "cleanup-shared-image-preserved")
    assert len(json.loads(result.stdout)["Images"]["Image"]) == 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, action="append", required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--operator-cidr", required=True)
    parser.add_argument("--ca", default="confidential-agent")
    parser.add_argument("--aliyun", default="aliyun")
    parser.add_argument("--parallel", type=int, choices=range(1, 7), default=3)
    parser.add_argument("--allow-model-skip", action="store_true",
                        help="Explicitly accept fixtures without real model calls")
    parser.add_argument("--concurrent-deploy", action="store_true")
    parser.add_argument("--interrupt-infrastructure", action="store_true")
    parser.add_argument("--resume-deploy", action="store_true")
    parser.add_argument("--frozen-inputs", action="store_true")
    parser.add_argument("--console-isolation", action="store_true")
    parser.add_argument("--application-errors", action="store_true")
    parser.add_argument("--stable-endpoints-fixture", type=Path)
    parser.add_argument("--incompatible-release", type=Path)
    parser.add_argument("--publication-state", type=Path)
    parser.add_argument("--software-versions", type=Path, help="Assert exact versions inside the shared image")
    parser.add_argument("--pty", action="store_true")
    parser.add_argument("--reboot", action="store_true",
                        help="Force-reboot one consumer and verify encrypted data survives")
    parser.add_argument("--trustee-url", help="Guest-visible URL for a disposable Trustee")
    parser.add_argument("--trustee-management-url")
    parser.add_argument("--trustee-admin-key", type=Path)
    parser.add_argument("--trustee-outage-service",
                        help="Stop this disposable local Trustee unit during a cold-boot test")
    parser.add_argument("--trustee-outage-control", type=Path,
                        help="Owned Trustee lifecycle executable, invoked with stop/start")
    args = parser.parse_args()
    if bool(args.trustee_url) != bool(args.trustee_admin_key):
        parser.error("--trustee-url and --trustee-admin-key must be supplied together")
    if (args.trustee_outage_service or args.trustee_outage_control) and not args.trustee_url:
        parser.error("Trustee outage coverage requires Trustee fixtures")
    if args.trustee_outage_service and args.trustee_outage_control:
        parser.error("choose one Trustee lifecycle control")
    if args.concurrent_deploy and args.resume_deploy:
        parser.error("choose concurrent or resumable first deployment")
    if args.resume_deploy and args.trustee_url:
        parser.error("resource-delivery resume coverage uses challenge mode")
    os.umask(0o077)
    args.work_dir = args.work_dir.resolve()
    args.work_dir.mkdir(mode=0o700, exist_ok=True)
    shared_state = args.work_dir / "state"
    consumers = [Consumer(path.resolve(), args.work_dir, args.ca, args.operator_cidr, shared_state)
                 for path in args.fixture]
    assert len(consumers) >= 2, "reuse requires at least two consumers"
    assert len({item.service for item in consumers}) == len(consumers)
    if args.stable_endpoints_fixture:
        extra_spec = json.loads((args.stable_endpoints_fixture / "appspec.json").read_text())
        stable_endpoint_peers(consumers, extra_spec)
    pins = [item.spec["deploy"]["image"] for item in consumers]
    assert all(pin == pins[0] for pin in pins), "fixtures must select the same release"
    assert all("build" not in item.spec and item.spec["service"]["ssh"] for item in consumers)
    if not args.allow_model_skip:
        assert all(item.probe.get("command") for item in consumers), "model probes are required; use --allow-model-skip to report exclusions"
    if args.trustee_url:
        assert all(item.spec["attestation"]["mode"] == "trustee" for item in consumers)
        configure = ["trustee", "configure", "--url", args.trustee_url,
                     "--admin-key", str(args.trustee_admin_key.resolve())]
        if (shared_state / "trustee/config.json").exists():
            configure += ["--force"]
        if args.trustee_management_url:
            configure += ["--management-url", args.trustee_management_url]
        first = consumers[0]
        first.command("trustee-configure", *configure)
        doctor = json.loads(first.command("trustee-doctor", "trustee", "doctor", "--json").stdout)
        if not doctor["owned_resource_policy"]:
            first.command("trustee-adopt", "trustee", "adopt", "--attestation-policy-sha256",
                          doctor["attestation_policy_sha256"], "--resource-policy-sha256",
                          doctor["resource_policy_sha256"])
        # One CA state owns the shared Trustee policies; serialize its updates.
        args.parallel = 1
    else:
        assert all(item.spec["attestation"]["mode"] == "challenge" for item in consumers)
    try:
        consumers[0].command("peering", "peering", "add", "--role", "operator",
                             "--cidr", args.operator_cidr, "--label", "reuse-test")
        reject_invalid_release(consumers[0])
        if args.incompatible_release:
            reject_incompatible_release(consumers[0], args.incompatible_release)
        if args.interrupt_infrastructure:
            interrupt_infrastructure(consumers[0], args.aliyun)
        if args.frozen_inputs or args.concurrent_deploy or args.resume_deploy:
            consumers[0].deploy(concurrent=args.concurrent_deploy, resume=args.resume_deploy)
            if args.concurrent_deploy:
                assert_one_deployment(consumers[0], args.aliyun)
            if args.frozen_inputs:
                deploy_with_frozen_inputs(consumers, args.parallel, args.aliyun)
            else:
                with ThreadPoolExecutor(max_workers=args.parallel) as pool:
                    list(pool.map(lambda item: item.deploy(), consumers[1:]))
        else:
            with ThreadPoolExecutor(max_workers=args.parallel) as pool:
                list(pool.map(lambda item: item.deploy(), consumers))
        assert_independent(consumers)
        if args.software_versions:
            expected = json.loads(args.software_versions.read_text())
            consumers[0].verify_software_versions(expected)
        if args.application_errors:
            consumers[0].verify_management_during_application_errors()
            for item in consumers:
                if any(resource["target"].endswith("/.codex/app-server-token") for resource in item.spec["resources"].values()):
                    item.verify_management_during_token_errors()
        if args.stable_endpoints_fixture:
            verify_stable_endpoints(consumers, args.stable_endpoints_fixture)
        if args.console_isolation:
            consumers[0].verify_console_isolation(args.aliyun)
        if args.pty:
            for item in consumers:
                item.verify_pty()
        if args.reboot:
            reboot(consumers[0], args.aliyun)
            for consumer in consumers[1:]:
                consumer.ssh("independent-data", "test ! -e /opt/data/allinone/reboot-marker")
        if args.trustee_outage_service or args.trustee_outage_control:
            trustee_outage(consumers[0], args.aliyun, args.trustee_outage_service,
                           str(args.trustee_outage_control) if args.trustee_outage_control else None)
        if args.publication_state:
            verify_publication_cleanup(consumers, args.publication_state, args.aliyun)
        first, *remaining = consumers
        first.destroy()
        first.attempted = False
        for item in remaining:
            item.ssh("survives-other-destroy", "cai-agent --version")
    finally:
        cleanup_errors = []
        for item in consumers:
            try:
                item.destroy()
            except Exception as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            raise RuntimeError("; ".join(cleanup_errors))
    image = json.loads((consumers[0].directory / "manifest.json").read_text())["image"]
    region = consumers[0].spec["deploy"]["region"]
    result = run([args.aliyun, "--region", region, "ecs", "DescribeImages", "--RegionId",
                  region, "--ImageId", image["image_id"]],
                 args.work_dir / "shared-image-preserved")
    assert len(json.loads(result.stdout)["Images"]["Image"]) == 1
    (args.work_dir / "result.json").write_text(json.dumps({
        "passed": True, "consumers": [item.service for item in consumers],
        "shared_image_preserved": True,
        "shared_workspace": True,
        "concurrent_deploy": args.concurrent_deploy,
        "interrupt_infrastructure": args.interrupt_infrastructure,
        "resume_deploy": args.resume_deploy,
        "frozen_inputs": args.frozen_inputs,
        "console_isolation": args.console_isolation,
        "application_errors": args.application_errors,
        "stable_endpoints": bool(args.stable_endpoints_fixture),
        "incompatible_runtime_rejected": bool(args.incompatible_release),
        "cleanup_consumers_and_failure_status": bool(args.publication_state),
        "pty": args.pty,
        "trustee_outage": bool(args.trustee_outage_service or args.trustee_outage_control),
        "model_calls": {item.service: "passed" if item.probe.get("command")
                        else {"skipped": item.probe["skip_reason"]} for item in consumers}
    }) + "\n")
    print("Shared-image consumer tests passed; private evidence:", args.work_dir)


if __name__ == "__main__":
    main()
