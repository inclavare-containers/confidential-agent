"""The third Trustee GPU owns single-node checks and shares the group image."""
import json
import copy
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_case


class InferenceIsolationTests(unittest.TestCase):
    def test_beta_combines_single_oneclick_and_trustee_failures_without_another_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            group, beta = "inference-reuse-nlb", "vllm-inference"
            for service in [group, beta]:
                service_dir = root / "state/services" / service
                (service_dir / "secrets").mkdir(parents=True)
                (service_dir / "manifest.json").write_text(json.dumps({"image": {"image_id": "m-shared"}}))
                for name in ["disk_passphrase", "gateway_identity.seed"]:
                    (service_dir / "secrets" / name).write_text(service + name)
            states = {group: {"deploy": {"instance_public_ips": ["192.0.2.1", "192.0.2.2"]}},
                      beta: {"deploy": {"public_ip": "192.0.2.3", "instance_id": "i-beta"}}}
            case = SimpleNamespace(work=root, state=root / "state", local_state=states.__getitem__,
                connect=Mock(return_value="beta-front"), record_resources=Mock(), command=Mock(), checks={},
                api=Mock(return_value={"Images": {"Image": [{"ImageId": "m-shared"}]}}))
            spec = root / "beta.json"
            calls = {"poll": 0}

            def eventually(probe, timeout, interval=5):
                calls["poll"] += 1
                self.assertTrue(probe())

            def config(address):
                if address == "192.0.2.3":
                    return "other-service-key"
                return "group-key-rotated" if calls["poll"] > 1 else "group-key-original"

            with patch.object(inference_case, "oneclick", return_value=(beta, spec)) as oneclick, \
                    patch.object(inference_case, "validate_live") as live, \
                    patch.object(inference_case, "infer", return_value=b"real-response") as infer, \
                    patch.object(inference_case, "reboot") as reboot, \
                    patch.object(inference_case, "validate_debug_gpu") as diagnostics, \
                    patch.object(inference_case, "wrong_model_key") as wrong, \
                    patch.object(inference_case, "trustee_outage") as outage, \
                    patch.object(inference_case, "verify_surviving_consumer") as survivor, \
                    patch.object(inference_case, "key_config", side_effect=config), \
                    patch.object(inference_case, "eventually", side_effect=eventually):
                inference_case.isolate_service(case, group, {"i-first": "node-1", "i-second": "node-2"}, "group-front", "trustee", "debug")
            oneclick.assert_called_once_with(case, 1, "debug", True, "nlb", "ohttp", "trustee")
            live.assert_called_once_with(case, beta, 1)
            reboot.assert_called_once_with(case, beta, spec, "trustee", "i-beta")
            diagnostics.assert_called_once_with(case, beta)
            wrong.assert_called_once_with(case, beta, spec, "i-beta", "beta-front")
            outage.assert_called_once_with(case, beta, spec, "i-beta", "beta-front", warm_front="group-front")
            survivor.assert_called_once_with(case, beta, states[beta], "beta-front")
            self.assertIn(("beta-front", True), [call.args for call in infer.call_args_list])
            self.assertEqual([call.args[2] for call in case.command.call_args_list], [group, beta])
            self.assertEqual(case.api.call_count, 2)
            self.assertTrue(case.checks["consumer_destruction_preserves_published_image"])
            self.assertTrue(case.checks["shared_image_service_isolation"])
            self.assertTrue(case.checks["ohttp_rotation"])
            proof = json.loads((root / "key-isolation.json").read_text())
            self.assertNotEqual(proof["group_key_sha256"], proof["other_service_key_sha256"])

    def test_consumer_destroy_requires_the_publisher_image_to_survive(self):
        case = SimpleNamespace(record_resources=Mock(), command=Mock(), checks={},
            api=Mock(return_value={"Images": {"Image": []}}))
        with self.assertRaisesRegex(AssertionError, "removed the published image"):
            inference_case.destroy_published_consumer(case, "consumer", "m-shared")
        self.assertNotIn("consumer_destruction_preserves_published_image", case.checks)

    def test_isolation_survivor_waits_for_new_generation_and_refreshes_existing_tunnel(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            group, beta = "inference-reuse-nlb", "vllm-inference"
            for service in (group, beta):
                state = root / "state/services" / service
                (state / "secrets").mkdir(parents=True)
                (state / "manifest.json").write_text('{"image":{"image_id":"m-shared"}}')
                for name in ("disk_passphrase", "gateway_identity.seed"):
                    (state / "secrets" / name).write_text(service + name)
            deploy = {"instance_id": "i-beta", "public_ip": "192.0.2.3", "resource_name": "beta-run"}
            states = {group: {"deploy": {"instance_public_ips": ["192.0.2.1", "192.0.2.2"]}},
                      beta: {"phase": "active", "deploy": deploy, "mesh_generation": 2}}
            counters = {"poll": 0, "status": 0, "post_destroy_infer": 0}
            events = []

            def command(label, *args, **options):
                if args[0] == "destroy" and args[1] == group:
                    states[beta]["mesh_generation"] = 3
                    events.append("destroy-primary")
                if args[0] == "status":
                    counters["status"] += 1
                    generation = 2 if counters["status"] == 1 else 3
                    events.append("status-" + str(generation))
                    return SimpleNamespace(stdout=json.dumps([{
                        "local": {"service_id": beta, "phase": "active", "cloud": deploy, "mesh_generation": 3},
                        "replicas": [{"instance_id": "i-beta", "daemon": {"service_id": beta,
                            "phase": "running", "app_ready": True, "mesh_ready": True, "mesh_generation": generation}}]}]).encode())
                return SimpleNamespace(stdout=b"")

            def infer(endpoint, stream=False, **options):
                if "destroy-primary" in events:
                    counters["post_destroy_infer"] += 1
                    events.append("infer-survivor")
                    if counters["post_destroy_infer"] == 1:
                        raise urllib.error.HTTPError(endpoint, 500, "key changed", {}, io.BytesIO(
                            b'{"code":"ShouldRequestNewKeyConfigFromServerError"}'))
                    self.assertEqual(endpoint, "beta-front")
                return b'{"model":"real-response"}'

            def poll(probe, timeout, interval=5):
                counters["poll"] += 1
                self.assertTrue(probe())

            def key(address):
                return "beta-key" if address.endswith(".3") else "group-rotated" if counters["poll"] > 1 else "group-original"

            case = SimpleNamespace(work=root, state=root / "state", local_state=lambda name: copy.deepcopy(states[name]),
                connect=Mock(return_value="beta-front"), record_resources=Mock(), command=command, checks={},
                api=Mock(return_value={"Images": {"Image": [{"ImageId": "m-shared"}]}}))
            with patch.object(inference_case, "oneclick", return_value=(beta, root / "spec.json")), \
                    patch.object(inference_case, "validate_live"), patch.object(inference_case, "reboot"), \
                    patch.object(inference_case, "validate_debug_gpu"), patch.object(inference_case, "wrong_model_key"), \
                    patch.object(inference_case, "trustee_outage"), patch.object(inference_case, "key_config", side_effect=key), \
                    patch.object(inference_case, "eventually", side_effect=poll), \
                    patch.object(inference_case, "infer", side_effect=infer), patch.object(inference_case.time, "sleep"):
                inference_case.isolate_service(case, group, {}, "group-front", "trustee", "debug")
            self.assertLess(events.index("status-3"), events.index("infer-survivor"))
            self.assertEqual(counters["post_destroy_infer"], 2)
            self.assertTrue(case.checks["destroying_group_preserves_other_service"])
            case.connect.assert_called_once_with(beta, "isolated-connect")


if __name__ == "__main__":
    unittest.main()
