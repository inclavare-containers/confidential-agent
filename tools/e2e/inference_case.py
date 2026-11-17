#!/usr/bin/env python3.11
"""Real encrypted-model inference over protected single-node and replica routes."""
import base64
import json
import hashlib
import os
from pathlib import Path
import re
import shutil
import time
import urllib.error
import urllib.request
from cloud_case import Case, ROOT, eventually, release_selection, run, write_json
from inference_fixtures import MODEL, debug_identity
from metrics_probe import complete_metrics


def request(base, path, body=None, *, timeout=180):
    payload = json.dumps(body).encode() if body is not None else None
    message = urllib.request.Request(base.rstrip("/")+path, data=payload,
                                     headers={"Content-Type": "application/json", "Connection": "close"})
    with urllib.request.urlopen(message, timeout=timeout) as response:
        return response.read()


def infer(base, stream=False, *, timeout=None):
    body = {"model": MODEL, "messages": [{"role": "user", "content": "Reply with INFERENCE_ and OK concatenated without spaces or any other text."}],
            "temperature": 0, "max_tokens": 64, "stream": stream,
            "chat_template_kwargs": {"enable_thinking": False}}
    response = request(base, "/v1/chat/completions", body, **({"timeout": timeout} if timeout is not None else {}))
    if stream:
        packets = [line[6:] for line in response.decode().splitlines() if line.startswith("data: ")]
        assert "[DONE]" in packets, "stream did not complete"
        content = "".join(json.loads(packet)["choices"][0].get("delta", {}).get("content", "") or ""
                          for packet in packets if packet != "[DONE]" and json.loads(packet).get("choices"))
    else:
        data = json.loads(response)
        assert data["model"] == MODEL and data["choices"]
        content = data["choices"][0]["message"]["content"]
    assert "INFERENCE_OK" in content, "real model response marker is missing"
    return response


def request_count(base, evidence):
    metrics = complete_metrics(base, evidence).decode()
    values = re.findall(r"^vllm:request_success_total(?:\{[^\n]*\})?\s+([0-9.eE+-]+)", metrics, re.M)
    assert values, "vLLM request counter is unavailable"
    return sum(float(value) for value in values)


def validate_live(case, service, expected):
    data = json.loads(case.command("status-live", "status", "--service", service, "--live", "--json").stdout)
    view = data[0]
    nodes = view.get("replicas", [])
    assert len(nodes) == expected
    assert all(node.get("daemon", {}).get("app_ready") and node["daemon"].get("mesh_ready") for node in nodes)
    assert not view.get("live_error")
    return nodes


def validate_lb_ingress(case, service, kind):
    deployment = case.local_state(service)["deploy"]
    directory = Path(deployment["terraform_dir"])
    groups, balancers = set(), set()
    for state in directory.rglob("*.tfstate"):
        for resource in json.loads(state.read_text()).get("resources", []):
            if resource.get("mode") != "managed":
                continue
            target = (groups if resource["type"] == "alicloud_security_group" and resource["name"] == kind + "_ingress"
                      else balancers if resource["type"] == "alicloud_" + kind + "_load_balancer" and resource["name"] == "shelter" else None)
            if target is not None:
                target.update(instance["attributes"]["id"] for instance in resource.get("instances", []))
    assert len(groups) == len(balancers) == 1, kind + " must own a dedicated ingress security group"
    group, balancer = next(iter(groups)), next(iter(balancers))
    attached = case.api(kind, "GetLoadBalancerAttribute", LoadBalancerId=balancer)
    attached_groups = attached.get("SecurityGroupIds", [])
    assert group in attached_groups, "owned ingress group is not attached"
    if kind == "nlb":
        assert set(attached_groups) == {group}, "NLB must bind exactly its configured ingress group"
    response = case.api("ecs", "DescribeSecurityGroupAttribute", SecurityGroupId=group, Direction="ingress")
    rules = response["Permissions"]["Permission"]
    assert any(rule["Policy"].lower() == "drop" and int(rule["Priority"]) == 100
               and rule["IpProtocol"].lower() == "all" and rule["PortRange"] == "-1/-1"
               and rule.get("SourceCidrIp") == "0.0.0.0/0" for rule in rules), "ingress lacks an explicit default deny"
    allowed = [rule for rule in rules if rule["Policy"].lower() == "accept"]
    assert allowed and all(int(rule["Priority"]) == 1 and rule["IpProtocol"].lower() == "tcp"
                           and rule["PortRange"] == "8080/8080" and rule.get("SourceCidrIp") == case.operator_cidr
                           for rule in allowed), "ingress is broader than the configured operator and port"
    write_json(case.work / (kind + "-ingress-policy.json"), {"security_group_id": group, "load_balancer_id": balancer,
               "operator_cidr": case.operator_cidr, "port": 8080, "default_deny": True, "attached": True,
               "auto_allocated_eip_ids": sorted({address["AllocationId"]
                   for zone in attached.get("ZoneMappings", []) for address in zone.get("LoadBalancerAddresses", [])
                   if address.get("AllocationId")}) if kind == "nlb" else []})
    case.checks[kind + "_owned_ingress_default_deny"] = True


def prepare(case, service, replicas, kind, transport, mode, reuse, variant):
    key = os.environ.get("E2E_MODEL_KEY_FILE")
    oss = os.environ.get("E2E_MODEL_OSS_BUCKET")
    if not key or not oss:
        raise RuntimeError("the encrypted Qwen3.8-27B fixture and model key are required")
    init = case.work / (service + "-init")
    cosign = case.work / "cosign.key"
    reference = ["--reference-values", "sample"]
    if not reuse:
        if not cosign.exists():
            case.command("cosign-key", "key", "generate-cosign", "--output-key-prefix", cosign.with_suffix(""))
        reference = ["--reference-values", "rekor", "--cosign-key", cosign]
    # Init supplies model delivery files only for consumers. Their actual JSON
    # spec below requires Rekor authenticated by the producer's signed release.
    case.command("init", "init", "vllm-inference", "--non-interactive", "--output-dir", init,
                 "--region", case.region, "--zone-id", os.environ.get("E2E_GPU_ZONE_ID", "cn-beijing-l"),
                 "--replicas", replicas, "--vllm-build-variants", variant, "--model-key-file", key,
                 "--oss-bucket", oss, *reference,
                 "--vllm-served-model-name", MODEL)
    project = init / "vllm-inference"
    if reuse:
        image = release_selection(case.suite / "prepare-inference-images" / ("inference-"+variant), case.region)
        spec = {"schema": "confidential-agent/v1", "service": {"id": service, "ports": [8080],
                "connect": [{"port": 8080, "encryption": transport}], "app_service": "cai-vllm.service"},
                "deploy": {"provider": "aliyun", "region": case.region,
                "zone_id": os.environ.get("E2E_GPU_ZONE_ID", "cn-beijing-l"), "instance_type": "ecs.gn8v-tee.4xlarge",
                "disk_gb": 512, "image": image, "image_variant": variant, "replicas": replicas},
                "attestation": {"tee": "tdx", "mode": mode, "reference_values": "rekor",
                                "rekor": {"slsa_generator": "/usr/libexec/shelter/slsa/slsa-generator"}},
                "resources": {
                    "model_decryption_key": {"source": str(project / "model-decryption-key"), "target": "/run/cai/secrets/model-decryption-key", "mode": "0600", "required": True},
                    "cai_model_env": {"source": str(project / "cai-model.env"), "target": "/etc/cai/model.env", "mode": "0600", "required": True}}}
        if replicas > 1:
            spec["deploy"]["slb"] = {"enabled": True, "slb_type": kind}
        path = project / "consumer.json"
        write_json(path, spec)
    else:
        path = project / "vllm-inference.yaml"
        yaml = path.read_text().replace("  id: vllm-inference", "  id: "+service)
        yaml = yaml.replace("encryption: ohttp", "encryption: "+transport).replace("slb_type: alb", "slb_type: "+kind)
        yaml = yaml.replace("  mode: challenge", "  mode: "+mode)
        path.write_text(yaml)
    return path


def key_config(address):
    message = urllib.request.Request("http://" + address + ":8080/", data=b'{"attestation_request":null}',
                                     headers={"Content-Type": "application/json", "x-tng-ohttp-api": "/tng/key-config"})
    with urllib.request.urlopen(message, timeout=20) as response:
        return json.loads(response.read())["hpke_key_config"]["encoded_key_config_list"]


def instance_status(case, instance):
    return case.api("ecs", "DescribeInstances", InstanceIds=json.dumps([instance]))["Instances"]["Instance"][0]["Status"]


def start_stopped_instance(case, instance):
    # A daemon becoming unreachable can precede the ECS stop transition.
    eventually(lambda: instance_status(case, instance) == "Stopped", 600)
    case.api("ecs", "StartInstance", InstanceId=instance)


def ssh_instance_id(case, service, instance):
    deployment = case.local_state(service)["deploy"]
    replicas = deployment.get("instance_ids") or []
    if replicas:
        assert instance in replicas, "SSH target is not a recorded replica"
        return instance
    assert instance == deployment.get("instance_id"), "SSH target is not the recorded single instance"
    return None


def validate_debug_gpu(case, service, instance_id=None):
    # NVIDIA exposes these states through conf-compute, not nvidia-smi -q.
    # https://docs.nvidia.com/cc-deployment-guide-snp.pdf
    for label, command, expected in [
        ("gpu-confidential-status", "nvidia-smi conf-compute -f", rb"CC\s+status\s*:\s*ON"),
        ("gpu-devtools-status", "nvidia-smi conf-compute -d", rb"DevTools\s+Mode\s*:\s*OFF"),
        ("gpu-vllm-version", "/root/.venv/bin/vllm --version", rb"0\.28\.0"),
    ]:
        evidence = label + "-" + service + ("-" + instance_id if instance_id else "")
        selector = ssh_instance_id(case, service, instance_id) if instance_id else None
        diagnostic = case.ssh(service, evidence, command, instance_id=selector,
                              identity_file=getattr(case, "debug_identity", None))
        assert re.fullmatch(expected, diagnostic.stdout.strip(), re.IGNORECASE), label + " did not match the required GPU runtime state"


def boot_id(case, service, instance, label):
    selector = ssh_instance_id(case, service, instance)
    value = case.ssh(service, label, "cat /proc/sys/kernel/random/boot_id", instance_id=selector,
                     identity_file=getattr(case, "debug_identity", None), timeout=30).stdout.strip()
    assert re.fullmatch(rb"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", value), "guest boot ID is invalid"
    return value.decode()


def changed_boot(case, service, instance, before, label):
    observed = {}
    def changed():
        value = boot_id(case, service, instance, label + "-boot-after")
        if value == before:
            return False
        observed["after"] = value
        return True
    eventually(changed, 7200)
    write_json(case.work / (label + "-boot-proof.json"), {
        "instance_id": instance, "before": before, "after": observed["after"]})
    return observed["after"]


def oneclick(case, replicas, variant, reuse=False, kind="alb", transport="ohttp", mode="challenge"):
    service = "vllm-inference"
    case.register(service)
    work = case.work / "oneclick"
    command = ["bash", ROOT / "one-click/install-vllm-inference.sh", "deploy", "--non-interactive", "--yes",
               "--state-dir", case.state, "--work-dir", work, "--region", case.region,
               "--zone-id", os.environ.get("E2E_GPU_ZONE_ID", "cn-beijing-l"), "--replicas", str(replicas),
               "--allowed-cidr", case.operator_cidr, "--model-key-file", os.environ["E2E_MODEL_KEY_FILE"],
               "--oss-bucket", os.environ["E2E_MODEL_OSS_BUCKET"], "--vllm-served-model-name", MODEL,
               "--vllm-build-variants", variant, "--no-start-connect", "--skip-cargo-build", "--skip-deps"]
    spec = work / "vllm-inference/vllm-inference.yaml"
    if reuse:
        spec = prepare(case, service, replicas, kind, transport, mode, True, variant)
        command += ["--image-spec", spec]
    environment = dict(os.environ, CA_INSTALL_CLI="0", CA_STATUS_TIMEOUT_SEC="7200")
    run(command, case.work / "oneclick-deploy", timeout=28800, environment=environment)
    case.checks["oneclick_entrypoint"] = True
    return service, spec


def connect_node(case, service, instance):
    ready = case.work / ("node-"+instance+"-ready.json")
    case.tunnels.append(ready)
    case.command("connect-node-"+instance, "connect", "start", "--service", service,
                 "--instance-id", instance, "--ready-json", ready, "--wait-ready", "180", timeout=240)
    return json.loads(ready.read_text())["client_endpoints"][0]["http_base_url"]


def reboot(case, service, spec, mode, instance):
    before = boot_id(case, service, instance, "reboot-boot-before")
    eventually(lambda: instance_status(case, instance) == "Running", 600)
    case.api("ecs", "RebootInstance", InstanceId=instance, ForceStop="true")
    time.sleep(30)
    recover_boot(case, service, spec, mode, instance, before, "reboot-" + instance)


def recover_boot(case, service, spec, mode, instance, before, label):
    if mode == "challenge":
        # Delivery recovery retains the exact infrastructure and shared service secrets.
        address = case.local_state(service)["deploy"]
        ids = address.get("instance_ids") or [address["instance_id"]]
        ips = address.get("instance_public_ips") or [address["public_ip"]]
        ip = ips[ids.index(instance)]
        # Inject already retries attested delivery and reconciles managed network
        # state. Give that one mutation the recovery budget: killing and replaying
        # it every 120 seconds can leave an authorized transaction interrupted.
        case.command("reboot-inject", "inject", "--spec", spec,
                     "--target-ip", ip, timeout=1200)
    eventually(lambda: bool(validate_live(case, service, len(case.local_state(service)["deploy"].get("instance_ids") or [instance]))), 7200)
    changed_boot(case, service, instance, before, label)


def recovered_inference(case, front, endpoints, instance):
    if endpoints:
        # The load balancer can keep routing to the healthy peer. Prove that
        # the exact stopped backend now serves a real protected model request.
        response = infer(endpoints[instance])
        (case.work / ("recovered-node-" + instance + ".json")).write_bytes(response)
    (case.work / "recovered-model-response.json").write_bytes(infer(front))
    if endpoints:
        case.checks["recovered_backend_instance_id"] = instance


def trustee_outage(case, service, spec, instance, front, warm_front=None):
    before = boot_id(case, service, instance, "trustee-outage-boot-before")
    case.trustee_action("stop")
    try:
        case.api("ecs", "RebootInstance", InstanceId=instance, ForceStop="true")
        time.sleep(30)
        for _ in range(8):
            selector = ssh_instance_id(case, service, instance)
            ssh = case.ssh(service, "trustee-outage-debug-ssh", "true", instance_id=selector,
                           identity_file=getattr(case, "debug_identity", None), check=False, timeout=30)
            assert ssh.returncode, "cold boot unlocked debug SSH without Trustee key release"
            status = json.loads(case.command("trustee-unavailable", "status", "--service", service, "--live", "--json").stdout)[0]
            assert status.get("live_error") and not status.get("daemon", {}).get("app_ready"), "cold boot bypassed Trustee"
            try:
                request(front, "/v1/models")
            except (OSError, urllib.error.URLError):
                pass
            else:
                raise AssertionError("cold boot served a model without Trustee key release")
            if warm_front:
                infer(warm_front)
            time.sleep(15)
        console = base64.b64decode(case.api("ecs", "GetInstanceConsoleOutput", InstanceId=instance)["ConsoleOutput"]).decode(errors="replace")
        (case.work / "trustee-outage-console.txt").write_text(console)
        assert "trustee" in console.lower() and "failed" in console.lower(), "missing cold-boot Trustee key-release failure evidence"
        if instance_status(case, instance) == "Running":
            case.api("ecs", "StopInstance", InstanceId=instance, ForceStop="true")
        eventually(lambda: instance_status(case, instance) == "Stopped", 600)
    finally:
        case.trustee_action("start")
    case.api("ecs", "StartInstance", InstanceId=instance)
    eventually(lambda: bool(validate_live(case, service, 1)), 7200)
    changed_boot(case, service, instance, before, "trustee-outage")
    infer(front)
    case.checks["trustee_cold_boot_fail_closed"] = True


def wrong_model_key(case, service, spec, instance, front):
    """A valid TEE cannot decrypt the encrypted model with an incorrect key."""
    config = json.loads(spec.read_text())
    source = Path(config["resources"]["model_decryption_key"]["source"])
    original = source.read_bytes()
    address = case.local_state(service)["deploy"]["public_ip"]
    before = boot_id(case, service, instance, "wrong-key-boot-before")
    wrong_boot = None
    try:
        source.write_text("deliberately-incorrect-e2e-model-key\n")
        case.command("wrong-key-delivery", "inject", "--spec", spec, "--target-ip", address)
        case.api("ecs", "RebootInstance", InstanceId=instance, ForceStop="true")
        wrong_boot = changed_boot(case, service, instance, before, "wrong-key")
        for _ in range(8):
            status = json.loads(case.command("wrong-key-status", "status", "--service", service, "--live", "--json").stdout)[0]
            assert not status.get("daemon", {}).get("app_ready"), "wrong model key produced a ready app"
            try:
                request(front, "/v1/models")
            except (OSError, urllib.error.URLError):
                pass
            else:
                raise AssertionError("wrong model key exposed the decrypted model")
            time.sleep(15)
    finally:
        source.write_bytes(original)
        case.command("restore-key-delivery", "inject", "--spec", spec, "--target-ip", address)
    # Reboot acknowledgement can precede shutdown and still expose the old
    # daemon's ready state. Complete the stop before checking cold-boot recovery.
    if instance_status(case, instance) == "Running":
        case.api("ecs", "StopInstance", InstanceId=instance, ForceStop="true")
    start_stopped_instance(case, instance)
    eventually(lambda: bool(validate_live(case, service, 1)), 7200)
    changed_boot(case, service, instance, wrong_boot or before, "restored-key")
    infer(front)
    case.checks["wrong_model_key_rejected"] = True


def destroy_published_consumer(case, service, image):
    case.record_resources()
    case.command("destroy-preserve-published-" + service, "destroy", service, timeout=1800)
    images = case.api("ecs", "DescribeImages", ImageId=image)["Images"]["Image"]
    assert len(images) == 1, "consumer destruction removed the published image"
    case.checks["consumer_destruction_preserves_published_image"] = True


def verify_surviving_consumer(case, service, before, front, timeout=300, interval=5):
    """Wait for revocation to reach the same guest, then prove protected inference."""
    deployment = before["deploy"]
    instance = deployment.get("instance_id")
    generation = before.get("mesh_generation")
    assert instance and type(generation) is int, "survivor lacks recorded instance or mesh generation"
    assert timeout > 0 and interval > 0
    deadline = time.monotonic() + timeout
    proof = {"service": service, "instance_id": instance, "before_mesh_generation": generation,
             "deployment_sha256": hashlib.sha256(json.dumps(deployment, sort_keys=True).encode()).hexdigest(),
             "complete": False, "attempts": []}
    try:
        while time.monotonic() < deadline:
            current = case.local_state(service)
            assert current["deploy"] == deployment, "survivor deployment identity changed"
            assert current.get("gateway_identity") == before.get("gateway_identity"), "survivor gateway identity changed"
            assert current.get("phase") == "active", "survivor is no longer active"
            committed = current.get("mesh_generation")
            assert type(committed) is int, "survivor has no committed mesh generation"
            attempt = {"local_mesh_generation": committed}
            proof["attempts"].append(attempt)
            result = case.command("survivor-status-" + str(len(proof["attempts"])), "status",
                "--service", service, "--live", "--json", timeout=min(60, deadline - time.monotonic()))
            views = json.loads(result.stdout)
            assert len(views) == 1, "survivor live status is missing or ambiguous"
            view = views[0]
            local = view["local"]
            assert local["service_id"] == service and local["phase"] == "active", "survivor live identity changed"
            for field in ("instance_id", "run_id", "resource_name", "terraform_dir", "public_ip", "private_ip"):
                assert local["cloud"].get(field) == deployment.get(field), "survivor live deployment changed: " + field
            nodes = view.get("replicas", [])
            assert len(nodes) == 1 and nodes[0].get("instance_id") == instance, "survivor replica identity changed"
            daemon = nodes[0].get("daemon") or {}
            if daemon:
                assert daemon.get("service_id") == service, "survivor daemon identity changed"
            attempt.update(live_mesh_generation=local.get("mesh_generation"),
                daemon_mesh_generation=daemon.get("mesh_generation"), phase=daemon.get("phase"),
                app_ready=daemon.get("app_ready"), mesh_ready=daemon.get("mesh_ready"),
                live_error=bool(view.get("live_error") or nodes[0].get("live_error")))
            ready = (committed > generation and local.get("mesh_generation") == committed
                     and daemon.get("mesh_generation") == committed and daemon.get("phase") == "running"
                     and daemon.get("app_ready") is True and daemon.get("mesh_ready") is True
                     and not attempt["live_error"])
            remaining = deadline - time.monotonic()
            if ready and remaining > 0:
                try:
                    response = infer(front, timeout=min(180, remaining))
                except urllib.error.HTTPError as error:
                    raw = error.read(16385)
                    try:
                        code = json.loads(raw).get("code") if len(raw) <= 16384 else None
                    except (ValueError, AttributeError):
                        code = None
                    attempt.update(http_status=error.code, ohttp_error_code=code if isinstance(code, str) and len(code) < 128 else None)
                    # TNG invalidates its old HPKE key for the next request.
                    # Other HTTP failures and incorrect model output fail here.
                    if error.code != 500 or code != "ShouldRequestNewKeyConfigFromServerError":
                        raise
                else:
                    (case.work / "survivor-model-response.json").write_bytes(response)
                    proof.update(complete=True, mesh_generation=committed)
                    return
            time.sleep(min(interval, max(0, deadline - time.monotonic())))
        raise RuntimeError("survivor did not converge and serve the model within its readiness budget")
    finally:
        write_json(case.work / "survivor-after-destroy.json", proof)


def isolate_service(case, service, endpoints, front, mode, variant):
    # The one-click entry point owns vllm-inference. The replica group has its
    # lane ID, so beta can exercise the single-node installer in the same state.
    beta, spec = oneclick(case, 1, variant, True, "nlb", "ohttp", mode)
    validate_live(case, beta, 1)
    beta_front = case.connect(beta, "isolated-connect")
    (case.work / "isolated-model-response.json").write_bytes(infer(beta_front))
    (case.work / "isolated-stream-response.sse").write_bytes(infer(beta_front, True))
    a = case.local_state(service)["deploy"]
    b = case.local_state(beta)["deploy"]
    addresses = a["instance_public_ips"]
    beta_address = b["public_ip"]
    primary_image = json.loads((case.state / "services" / service / "manifest.json").read_text())["image"]["image_id"]
    beta_image = json.loads((case.state / "services" / beta / "manifest.json").read_text())["image"]["image_id"]
    assert primary_image == beta_image, "isolated services did not reuse the identical published image"
    reboot(case, beta, spec, mode, b["instance_id"])
    infer(beta_front)
    if variant == "debug":
        validate_debug_gpu(case, beta)
    if mode == "trustee":
        wrong_model_key(case, beta, spec, b["instance_id"], beta_front)
        trustee_outage(case, beta, spec, b["instance_id"], beta_front, warm_front=front)
    keys = {}
    def converged():
        group = [key_config(address) for address in addresses]
        other = key_config(beta_address)
        if len(set(group)) != 1 or group[0] == other:
            return False
        keys.update(a=group[0], b=other)
        return True
    eventually(converged, 600)
    secrets = case.state / "services"
    for name in ["disk_passphrase", "gateway_identity.seed"]:
        assert (secrets / service / "secrets" / name).read_bytes() != (secrets / beta / "secrets" / name).read_bytes()
    original = keys["a"]
    def rotated():
        if not converged() or keys["a"] == original:
            return False
        for endpoint in [front, *endpoints.values(), beta_front]:
            infer(endpoint)
        return True
    eventually(rotated, 900, 10)
    write_json(case.work / "key-isolation.json", {"rotation_verified": True,
               "group_key_sha256": hashlib.sha256(keys["a"].encode()).hexdigest(),
               "other_service_key_sha256": hashlib.sha256(keys["b"].encode()).hexdigest()})
    survivor = case.local_state(beta)
    destroy_published_consumer(case, service, primary_image)
    verify_surviving_consumer(case, beta, survivor, beta_front)
    destroy_published_consumer(case, beta, beta_image)
    case.checks.update(shared_image_service_isolation=True, ohttp_rotation=True,
                       destroying_group_preserves_other_service=True)


def main():
    replicas = int(os.environ.get("E2E_REPLICAS", "1"))
    kind = os.environ.get("E2E_SLB_TYPE", "nlb")
    transport = os.environ.get("E2E_CONNECT_ENCRYPTION", "ohttp")
    mode = os.environ.get("E2E_ATTESTATION_MODE", "challenge")
    reuse = os.environ.get("E2E_REUSE_IMAGE") == "1"
    variant = os.environ.get("E2E_IMAGE_VARIANT", "debug" if reuse else "release")
    service = os.environ.get("E2E_CASE_ID", "inference")
    with Case() as case:
        if variant == "debug" and reuse:
            # Fail before operator discovery, Trustee setup or any cloud call.
            case.debug_identity = debug_identity(case)
        case.operator()
        if mode == "trustee":
            case.trustee()
        if (not reuse and kind == "alb") or os.environ.get("E2E_ONECLICK") == "1":
            service, spec = oneclick(case, replicas, variant, reuse, kind, transport, mode)
        else:
            spec = prepare(case, service, replicas, kind, transport, mode, reuse, variant)
            case.command("validate", "spec", "validate", "--spec", spec)
            if not reuse:
                case.register(service)
                case.command("build", "build", "--spec", spec, timeout=28800)
            case.deploy(service, spec)
        nodes = validate_live(case, service, replicas)
        if replicas > 1:
            validate_lb_ingress(case, service, kind)
        deployment = case.local_state(service)["deploy"]
        ids = deployment.get("instance_ids") or [deployment["instance_id"]]
        endpoints = {instance: connect_node(case, service, instance) for instance in ids} if replicas > 1 else {}
        front = case.connect(service)
        for index, endpoint in enumerate([front, *endpoints.values()]):
            models = json.loads(request(endpoint, "/v1/models"))
            assert MODEL in [model["id"] for model in models["data"]]
            (case.work / ("model-response-"+str(index)+".json")).write_bytes(infer(endpoint))
            (case.work / ("stream-response-"+str(index)+".sse")).write_bytes(infer(endpoint, True))
        if replicas > 1:
            before = {instance: request_count(endpoint, case.work / ("metrics-before-" + instance))
                      for instance, endpoint in endpoints.items()}
            for index in range(replicas * 8):
                if kind == "nlb":
                    # NLB balances TCP connections. A multiplexed RATS-TLS
                    # session legitimately stays on one backend, so use a new
                    # attested client connection for each distribution sample.
                    label = "distribution-" + str(index)
                    endpoint = case.connect(service, label)
                    try:
                        infer(endpoint)
                    finally:
                        case.command("stop-" + label, "connect", "stop", "--ready-json",
                                     case.work / (label + "-ready.json"))
                else:
                    infer(front)
            after = {instance: request_count(endpoint, case.work / ("metrics-after-" + instance))
                     for instance, endpoint in endpoints.items()}
            assert all(after[instance] > before[instance] for instance in ids), "LB traffic did not reach every replica"
            write_json(case.work / "distribution.json", {"before": before, "after": after})
            # Reject topology changes before touching any existing resource.
            if spec.suffix == ".json":
                modified = json.loads(spec.read_text())
                modified["deploy"]["replicas"] = replicas+1
                invalid = spec.with_name("changed-topology.json")
                write_json(invalid, modified)
            else:
                invalid = spec.with_name("changed-topology.yaml")
                invalid.write_text(spec.read_text().replace("  replicas: "+str(replicas), "  replicas: "+str(replicas+1)))
            case.command("validate-changed-topology", "spec", "validate", "--spec", invalid)
            rejected = case.command("reject-topology-inject", "inject", "--spec", invalid,
                                    "--target-ip", deployment["instance_public_ips"][0], check=False)
            assert rejected.returncode and b"cannot change the deployed replica count" in rejected.stderr
            for label, arguments in [("deploy", []), ("resume", ["--resume"])]:
                assert case.command("reject-topology-"+label, "deploy", *arguments, "--spec", invalid, check=False).returncode
            assert case.local_state(service)["deploy"] == deployment
            # One failed node must be visible even while the remaining node serves requests.
            before_boot = boot_id(case, service, ids[0], "degraded-boot-before")
            case.api("ecs", "StopInstance", InstanceId=ids[0], ForceStop="true")
            def degraded():
                status = json.loads(case.command("degraded-status", "status", "--service", service, "--live", "--json").stdout)[0]
                return bool(status.get("live_error")) and not status.get("daemon", {}).get("app_ready")
            eventually(degraded, 300)
            eventually(lambda: bool(infer(front)), 300)
            start_stopped_instance(case, ids[0])
            recover_boot(case, service, spec, mode, ids[0], before_boot, "degraded-recovery-" + ids[0])
        else:
            reboot(case, service, spec, mode, ids[0])
        recovered_inference(case, front, endpoints, ids[0])
        if variant == "release":
            assert not any(node["daemon"].get("debug_ssh_ready") for node in nodes)
        else:
            for instance in ids:
                validate_debug_gpu(case, service, instance_id=instance)
        if mode == "trustee" and replicas == 1:
            wrong_model_key(case, service, spec, ids[0], front)
            trustee_outage(case, service, spec, ids[0], front)
        if os.environ.get("E2E_SERVICE_ISOLATION") == "1":
            isolate_service(case, service, endpoints, front, mode, variant)
        elif reuse:
            image = json.loads((case.state / "services" / service / "manifest.json").read_text())["image"]["image_id"]
            destroy_published_consumer(case, service, image)
        case.checks.update(real_model=True, streaming=True, per_replica=True, replicas=replicas,
                           transport=transport, load_balancer=kind if replicas > 1 else None,
                           attestation=mode, source="published-image" if reuse else "build", reboot=True)


if __name__ == "__main__":
    main()
