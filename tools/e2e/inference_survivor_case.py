#!/usr/bin/env python3.11
"""Focused Trustee/NLB survivor reconfiguration; separate from the full matrix."""
import hashlib
import json
import os

from cloud_case import Case, eventually, release_selection, write_json
from inference_case import (connect_node, destroy_published_consumer, infer, key_config,
                            oneclick, prepare, validate_lb_ingress, validate_live,
                            verify_surviving_consumer)
from inference_fixtures import debug_identity

SCOPE = "survivor-reconfiguration-only"


def main():
    with Case() as case:
        case.checks["scope"] = SCOPE
        scope = {"scope": SCOPE, "complete": False, "attestation": "trustee", "transport": "ohttp",
                 "topology": {"nlb_group_replicas": 2, "isolated_single_replicas": 1},
                 "excluded": ["reboot", "wrong-model-key", "trustee-outage", "natural-key-rotation",
                              "full-original-nlb-lane"]}
        write_json(case.work / "survivor-scope.json", scope)
        group = os.environ.get("E2E_CASE_ID", "inference-survivor-nlb")
        assert group != "vllm-inference", "replica group must have a distinct service ID"
        # Validate local fixture custody before discovering or allocating cloud resources.
        case.debug_identity = debug_identity(case)
        release_selection(case.suite / "prepare-inference-images/inference-debug", case.region)
        case.operator()
        case.trustee()
        spec = prepare(case, group, 2, "nlb", "ohttp", "trustee", True, "debug")
        case.command("validate-survivor-group", "spec", "validate", "--spec", spec)
        case.deploy(group, spec)
        # Beta joins as an authorized mesh peer and adds its own ingress /32.
        # Check the initial operator-only policy before that reconciliation.
        validate_lb_ingress(case, group, "nlb")
        beta, _ = oneclick(case, 1, "debug", True, "nlb", "ohttp", "trustee")
        assert beta != group
        eventually(lambda: bool(validate_live(case, group, 2)) and bool(validate_live(case, beta, 1)), 7200)
        primary, secondary = case.local_state(group)["deploy"], case.local_state(beta)["deploy"]
        instances, addresses = primary.get("instance_ids", []), primary.get("instance_public_ips", [])
        assert len(instances) == len(set(instances)) == len(addresses) == len(set(addresses)) == 2
        assert secondary.get("instance_id") and secondary["instance_id"] not in instances
        assert not secondary.get("slb_vip") and len(secondary.get("instance_ids", [])) <= 1
        manifests = case.state / "services"
        image = json.loads((manifests / group / "manifest.json").read_text())["image"]["image_id"]
        assert image == json.loads((manifests / beta / "manifest.json").read_text())["image"]["image_id"], \
            "focused consumers must reuse the identical signed debug image"
        for name in ("disk_passphrase", "gateway_identity.seed"):
            first = (manifests / group / "secrets" / name).read_bytes()
            second = (manifests / beta / "secrets" / name).read_bytes()
            assert first and second and first != second, "services did not receive distinct " + name
        keys = {}

        def isolated_keys():
            shared = [key_config(address) for address in addresses]
            other = key_config(secondary["public_ip"])
            if not all(shared) or not other or len(set(shared)) != 1 or shared[0] == other:
                return False
            keys.update(group=shared[0], beta=other)
            return True

        eventually(isolated_keys, 600)
        write_json(case.work / "key-isolation.json", {"scope": SCOPE, "image_id": image,
            "group_key_sha256": hashlib.sha256(keys["group"].encode()).hexdigest(),
            "other_service_key_sha256": hashlib.sha256(keys["beta"].encode()).hexdigest(),
            "disk_and_gateway_secrets_distinct": True})
        endpoints = {"group": case.connect(group, "survivor-group-connect")}
        endpoints.update({"node-" + str(index): connect_node(case, group, instance)
                          for index, instance in enumerate(instances)})
        beta_front = case.connect(beta, "isolated-connect")
        endpoints["beta"] = beta_front
        for label, endpoint in endpoints.items():
            (case.work / ("baseline-model-" + label + ".json")).write_bytes(infer(endpoint))
            (case.work / ("baseline-stream-" + label + ".sse")).write_bytes(infer(endpoint, True))
        survivor = case.local_state(beta)
        destroy_published_consumer(case, group, image)
        verify_surviving_consumer(case, beta, survivor, beta_front)
        destroy_published_consumer(case, beta, image)
        case.checks.update(real_model=True, streaming=True, per_replica=True, replicas=2,
                           transport="ohttp", load_balancer="nlb", attestation="trustee", source="published-image",
                           shared_image_service_isolation=True, destroying_group_preserves_other_service=True)
        scope["complete"] = True
        write_json(case.work / "survivor-scope.json", scope)


if __name__ == "__main__":
    main()
