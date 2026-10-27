#!/usr/bin/env python3.11
"""Six real harnesses on one signed image, with a clean consumer state."""
import json
import os
import socket
import urllib.error
import urllib.request
from cloud_case import Case, ROOT, eventually, release_selection, run
from fixtures.allinone import generate
from platform_case import add_platform_fixtures


def full_hardening_smoke(case):
    """A distinct FULL HARDENING image is usable through authenticated HTTP."""
    case.operator()
    producer = case.suite / "publish-reuse-images"
    image = release_selection(producer / "allinone-hardened", case.region)
    fixture, = generate(case.work / "fixtures", image, "challenge", case.region,
                        os.environ["E2E_CASE_ID"], harnesses=["hermes"], ssh=False)
    spec_path = fixture / "appspec.json"
    spec = json.loads(spec_path.read_text())
    probe = json.loads((fixture / "probe.json").read_text())
    service = spec["service"]["id"]
    case.command("strict-validate", "spec", "validate", "--spec", spec_path)
    case.deploy(service, spec_path)
    directory = case.state / "services" / service
    manifest = json.loads((directory / "manifest.json").read_text())
    contract = manifest["runtime_contract"]
    assert contract["image_variant"] == "release" and contract["rootfs_integrity"] is True
    assert contract["writable_layer_encryption"] == "luks2" and contract["key_release"] == "ca-attested"
    assert not any(value.startswith("instance-ssh") for value in contract["capabilities"])
    assert not manifest.get("ssh") and not manifest.get("debug_ssh")
    assert not any((directory / "secrets" / name).exists()
                   for name in ["ssh_client", "ssh_host", "debug_ssh"])
    assert not list(case.state.rglob("*.qcow2")), "strict consumer staged a local producer disk"
    rejected = case.command("strict-no-ssh", "ssh", service, "--", "true", check=False, timeout=30)
    assert rejected.returncode != 0 and b"no debug SSH key" in rejected.stderr
    public_ip = case.local_state(service)["deploy"]["public_ip"]
    try:
        with socket.create_connection((public_ip, 22), timeout=10):
            raise AssertionError("strict release exposes an SSH listener")
    except OSError:
        pass

    def attested_ready():
        report = json.loads(case.command("strict-report", "report", "--service", service, "--json").stdout)
        item, = report["services"]
        assert item["service_id"] == service and item["collect_status"] == "ok"
        assert item["tee_info"]["status"] == "ok" and item["tee_info"]["tee"] == "tdx"
        assert item["attestation"]["status"] == "ok" and item["attestation"]["ear_jwt"]
        assert item["daemon"]["app_ready"] and item["daemon"]["mesh_ready"]
        assert not item["daemon"].get("debug_ssh_ready")
        return True

    eventually(attested_ready, timeout=300)
    base = case.connect(service, "strict-http")
    for path in ["/v1/models", "/v1/chat/completions"]:
        request = urllib.request.Request(base + path,
            data=b'{}' if path.endswith("completions") else None,
            headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=30):
                raise AssertionError("strict HTTP endpoint accepted a missing capability token")
        except urllib.error.HTTPError as error:
            assert error.code in [401, 403], "unauthenticated request failed outside HTTP authorization"
    call = probe["http_model"]
    reply = run(["node", ROOT / "tools/e2e/probes/hermes-chat-probe.mjs", "--url", base,
                 "--token", call["token"], "--model", call["model"], "--message", call["message"],
                 "--expect", probe["expect"], "--timeout-ms", "600000"], case.work / "strict-model", timeout=660)
    result = json.loads(reply.stdout)
    assert result["ok"] and probe["expect"] in result["text"]
    case.checks.update(full_hardening_release=True, service_ssh=False, release_ssh_capabilities=False,
                       protected_http_unauthorized_rejected=True, protected_http_real_model=True,
                       release_signature_verified=True, attested_tdx_ready=True)


def main():
    mode = os.environ.get("E2E_ATTESTATION_MODE", "challenge")
    with Case() as case:
        if os.environ.get("E2E_ALLINONE_FULL_HARDENING") == "1":
            assert mode == "challenge", "strict release smoke uses Challenge attestation"
            full_hardening_smoke(case)
            return
        cidr = case.operator()
        producer = case.suite / "publish-reuse-images"
        release = producer / "allinone"
        image = release_selection(release, case.region)
        prefix = os.environ["E2E_CASE_ID"]
        fixtures = generate(case.work / "fixtures", image, mode, case.region, prefix,
                            harnesses=["openclaw", "codex"] if mode == "trustee" else None)
        extra = None
        if mode == "challenge":
            fixtures, extra = add_platform_fixtures(case.work, fixtures, prefix)
        state = case.work / "consumer" / "state"
        case.state = state
        info = case.trustee(configure=False) if mode == "trustee" else None
        for fixture in fixtures:
            service = json.loads((fixture / "appspec.json").read_text())["service"]["id"]
            case.register(service, state)
        if extra:
            service = json.loads((extra / "appspec.json").read_text())["service"]["id"]
            case.register(service, state)
            case.register("zzzz-" + service, state)
        command = ["python3.11", ROOT / "tools/e2e/allinone-reuse.py", "--work-dir", case.work / "consumer",
                   "--ca", case.ca, "--operator-cidr", cidr, "--parallel", "3", "--reboot", "--pty",
                   "--console-isolation", "--application-errors", "--publication-state", producer / "state/services/allinone-builder/state.json",
                   "--software-versions", ROOT / "software-versions.json"]
        for fixture in fixtures:
            command += ["--fixture", fixture]
        if info:
            # This CA state already owns this disposable Trustee's policies.
            command += ["--trustee-url", info["url"], "--trustee-admin-key", info["admin_key"]]
            command += ["--trustee-outage-control", ROOT / "tools/e2e/trustee_fixture.sh"]
        else:
            command += ["--concurrent-deploy", "--interrupt-infrastructure", "--resume-deploy",
                        "--frozen-inputs", "--stable-endpoints-fixture", extra]
        environment = dict(os.environ)
        if info:
            environment.update(E2E_TRUSTEE_WORK_DIR=str(case.trustee_work),
                               E2E_STATE_DIR=str(state), E2E_ALLOWED_CIDR=cidr)
        run(command, case.work / "reuse", timeout=21600, environment=environment)
        summary = json.loads((case.work / "consumer" / "result.json").read_text())
        assert summary["passed"] and all(value == "passed" for value in summary["model_calls"].values())
        assert len(summary["model_calls"]) == len(fixtures)
        if mode == "challenge":
            for name in ["concurrent_deploy", "interrupt_infrastructure", "resume_deploy",
                         "frozen_inputs", "stable_endpoints", "application_errors"]:
                assert summary[name] is True, name + " coverage did not execute"
                case.checks[name] = True
        else:
            assert summary["trustee_policy_lifecycle"] and summary["trustee_destroy_refusal"]
        case.checks.update(real_model_harnesses=[fixture.name for fixture in fixtures], signed_image_reused=True,
                           provider=mode, private_credentials=True)
        # Exact shared-image versions are authenticated by the runtime contract
        # and probed once; each fixture executes its selected native harness.


if __name__ == "__main__":
    main()
