"""The standalone regression keeps the original topology and narrow scope."""
import copy
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_survivor_case as driver


class FakeCase(SimpleNamespace):
    def __enter__(self):
        return self

    def __exit__(self, kind, error, traceback):
        self.cleanup(error)
        return False


class InferenceSurvivorDriverTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.group, self.beta = "focused-nlb", "vllm-inference"
        self.states = {
            self.group: {"deploy": {"instance_ids": ["i-first", "i-second"],
                "instance_public_ips": ["192.0.2.1", "192.0.2.2"]}},
            self.beta: {"phase": "active", "mesh_generation": 2, "gateway_identity": "beta-key",
                "deploy": {"instance_id": "i-beta", "public_ip": "192.0.2.3"}}}
        for service in (self.group, self.beta):
            state = self.root / "state/services" / service
            (state / "secrets").mkdir(parents=True)
            (state / "manifest.json").write_text('{"image":{"image_id":"m-shared"}}')
            for name in ("disk_passphrase", "gateway_identity.seed"):
                (state / "secrets" / name).write_text(service + name)
        self.case = FakeCase(work=self.root, state=self.root / "state", suite=self.root / "suite",
            region="cn-beijing", checks={}, operator=Mock(), trustee=Mock(), command=Mock(), deploy=Mock(),
            local_state=lambda name: copy.deepcopy(self.states[name]), cleanup=Mock(),
            connect=Mock(side_effect=lambda name, label: "beta-front" if name == self.beta else "group-front"))
        self.patches = [
            patch.dict(driver.os.environ, {"E2E_CASE_ID": self.group}),
            patch.object(driver, "Case", return_value=self.case),
            patch.object(driver, "debug_identity", return_value=self.root / "original-debug-key"),
            patch.object(driver, "release_selection", return_value={"release": "/signed-debug-release"}),
            patch.object(driver, "prepare", return_value=self.root / "group.json"),
            patch.object(driver, "oneclick", return_value=(self.beta, self.root / "beta.json")),
            patch.object(driver, "validate_live", return_value=[{}]),
            patch.object(driver, "validate_lb_ingress"),
            patch.object(driver, "eventually", side_effect=lambda probe, timeout: self.assertTrue(probe())),
            patch.object(driver, "key_config", side_effect=lambda address: "beta-ohttp" if address.endswith(".3") else "group-ohttp"),
            patch.object(driver, "connect_node", side_effect=lambda case, service, instance: "node-" + instance),
            patch.object(driver, "infer", return_value=b'{"model":"real-response"}'),
            patch.object(driver, "verify_surviving_consumer"),
            patch.object(driver, "destroy_published_consumer"),
        ]
        for context in self.patches:
            context.start()
            self.addCleanup(context.stop)

    def test_exact_topology_baseline_routes_original_tunnel_and_consumer_only_cleanup(self):
        order = []
        driver.destroy_published_consumer.side_effect = lambda case, service, image: order.append("destroy-" + service)
        driver.verify_surviving_consumer.side_effect = lambda *args: order.append("prove-survivor")
        driver.main()
        driver.prepare.assert_called_once_with(self.case, self.group, 2, "nlb", "ohttp", "trustee", True, "debug")
        driver.oneclick.assert_called_once_with(self.case, 1, "debug", True, "nlb", "ohttp", "trustee")
        driver.release_selection.assert_called_once_with(self.case.suite / "prepare-inference-images/inference-debug", "cn-beijing")
        self.case.trustee.assert_called_once_with()
        self.assertEqual([call.args[2] for call in driver.validate_live.call_args_list], [2, 1])
        self.assertEqual({call.args for call in driver.infer.call_args_list}, {
            (endpoint,) for endpoint in ("group-front", "node-i-first", "node-i-second", "beta-front")
        } | {(endpoint, True) for endpoint in ("group-front", "node-i-first", "node-i-second", "beta-front")})
        self.assertEqual(driver.infer.call_count, 8)
        driver.verify_surviving_consumer.assert_called_once_with(self.case, self.beta, self.states[self.beta], "beta-front")
        self.assertEqual([call.args for call in driver.destroy_published_consumer.call_args_list],
                         [(self.case, self.group, "m-shared"), (self.case, self.beta, "m-shared")])
        self.assertEqual(order, ["destroy-" + self.group, "prove-survivor", "destroy-" + self.beta])
        self.case.connect.assert_any_call(self.beta, "isolated-connect")
        self.assertEqual(self.case.connect.call_count, 2)
        self.assertEqual(self.case.checks["scope"], "survivor-reconfiguration-only")
        self.assertNotIn("reboot", self.case.checks)
        self.assertNotIn("ohttp_rotation", self.case.checks)
        self.assertTrue(json.loads((self.root / "survivor-scope.json").read_text())["complete"])
        self.case.cleanup.assert_called_once_with(None)

    def test_baseline_failure_exits_case_for_cleanup_without_claiming_survivor_success(self):
        driver.infer.side_effect = RuntimeError("real model unavailable")
        with self.assertRaisesRegex(RuntimeError, "real model unavailable"):
            driver.main()
        self.case.cleanup.assert_called_once()
        self.assertIsInstance(self.case.cleanup.call_args.args[0], RuntimeError)
        driver.verify_surviving_consumer.assert_not_called()
        driver.destroy_published_consumer.assert_not_called()
        self.assertNotIn("destroying_group_preserves_other_service", self.case.checks)
        self.assertFalse(json.loads((self.root / "survivor-scope.json").read_text())["complete"])

    def test_strict_ingress_is_checked_before_beta_adds_a_mesh_peer_rule(self):
        joined = False

        def deploy_beta(*args):
            nonlocal joined
            joined = True
            return self.beta, self.root / "beta.json"

        def operator_only_ingress(*args):
            self.assertFalse(joined, "beta's legitimate mesh-peer allow rule violates the operator-only check")

        driver.oneclick.side_effect = deploy_beta
        driver.validate_lb_ingress.side_effect = operator_only_ingress
        driver.main()
        driver.validate_lb_ingress.assert_called_once_with(self.case, self.group, "nlb")

    def test_mismatched_image_or_service_secrets_cannot_pass(self):
        for change in ("image", "secrets"):
            with self.subTest(change=change):
                manifest = self.root / "state/services" / self.beta / "manifest.json"
                manifest.write_text('{"image":{"image_id":"m-other"}}' if change == "image" else '{"image":{"image_id":"m-shared"}}')
                if change == "secrets":
                    (manifest.parent / "secrets/disk_passphrase").write_text(self.group + "disk_passphrase")
                with self.assertRaises(AssertionError):
                    driver.main()
                driver.infer.assert_not_called()
                driver.verify_surviving_consumer.assert_not_called()

    def test_missing_original_debug_key_fails_before_cloud_operations(self):
        driver.debug_identity.side_effect = RuntimeError("original debug key missing")
        with self.assertRaisesRegex(RuntimeError, "original debug key missing"):
            driver.main()
        self.case.operator.assert_not_called()
        self.case.trustee.assert_not_called()
        self.case.deploy.assert_not_called()


if __name__ == "__main__":
    unittest.main()
