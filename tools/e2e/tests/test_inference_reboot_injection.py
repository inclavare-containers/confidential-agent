"""A reboot recovery must not repeatedly interrupt a managed injection."""
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_case


class RebootInjectionTests(unittest.TestCase):
    def test_recovery_infers_through_the_exact_recovered_backend_before_the_lb(self):
        for recovered in ["i-first", "i-second"]:
            with self.subTest(recovered=recovered), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                case = SimpleNamespace(work=work, checks={})
                endpoints = {"i-first": "protected-first", "i-second": "protected-second"}
                with patch.object(inference_case, "infer", side_effect=[b"recovered backend", b"load balancer"]) as infer:
                    inference_case.recovered_inference(case, "protected-lb", endpoints, recovered)
                self.assertEqual([call.args for call in infer.call_args_list],
                                 [(endpoints[recovered],), ("protected-lb",)])
                self.assertEqual((work / ("recovered-node-" + recovered + ".json")).read_bytes(), b"recovered backend")
                self.assertEqual((work / "recovered-model-response.json").read_bytes(), b"load balancer")
                self.assertEqual(case.checks["recovered_backend_instance_id"], recovered)

    def test_recovery_does_not_fall_back_to_a_healthy_backend_or_the_lb(self):
        with tempfile.TemporaryDirectory() as directory:
            case = SimpleNamespace(work=Path(directory), checks={})
            with patch.object(inference_case, "infer") as infer:
                with self.assertRaises(KeyError):
                    inference_case.recovered_inference(case, "protected-lb", {"i-healthy": "protected-healthy"}, "i-recovered")
                infer.assert_not_called()
            self.assertNotIn("recovered_backend_instance_id", case.checks)
            with patch.object(inference_case, "infer", side_effect=RuntimeError("recovered backend failed")) as infer:
                with self.assertRaisesRegex(RuntimeError, "recovered backend failed"):
                    inference_case.recovered_inference(case, "protected-lb", {"i-recovered": "protected-recovered"}, "i-recovered")
                infer.assert_called_once_with("protected-recovered")
            self.assertFalse((case.work / "recovered-model-response.json").exists())
            self.assertNotIn("recovered_backend_instance_id", case.checks)

    @staticmethod
    def poll(probe, timeout):
        for _ in range(3):
            try:
                if probe():
                    return
            except (OSError, ValueError, RuntimeError, AssertionError):
                pass
        raise RuntimeError("readiness retries exhausted")

    def test_challenge_mutation_uses_original_phase_budget_once(self):
        events = []
        deployment = {"instance_ids": ["i-first", "i-second"],
                      "instance_public_ips": ["192.0.2.1", "192.0.2.2"]}

        def command(label, *args, **options):
            events.append("inject")
            self.assertEqual(label, "reboot-inject")
            self.assertEqual(args, ("inject", "--spec", Path("spec.json"),
                                    "--target-ip", "192.0.2.2"))
            # The CLI owns delivery retries; a provider observation lasting more
            # than 120 seconds must fit within the existing 1200-second phase.
            self.assertEqual(options, {"timeout": 1200})
            return SimpleNamespace(returncode=0)

        case = SimpleNamespace(command=Mock(side_effect=command),
                               api=Mock(side_effect=lambda *a, **kw: events.append("reboot")),
                               local_state=lambda _: {"deploy": deployment})
        with patch.object(inference_case, "instance_status", return_value="Running"), \
                patch.object(inference_case, "boot_id", return_value="old-boot"), \
                patch.object(inference_case, "changed_boot") as changed, \
                patch.object(inference_case.time, "sleep"), \
                patch.object(inference_case, "eventually", side_effect=self.poll), \
                patch.object(inference_case, "validate_live", side_effect=lambda *a: events.append("ready") or [{}]) as live:
            inference_case.reboot(case, "service", Path("spec.json"), "challenge", "i-second")
        self.assertEqual(events, ["reboot", "inject", "ready"])
        case.command.assert_called_once()
        case.api.assert_called_once_with("ecs", "RebootInstance", InstanceId="i-second", ForceStop="true")
        live.assert_called_once_with(case, "service", 2)
        changed.assert_called_once_with(case, "service", "i-second", "old-boot", "reboot-i-second")

    def test_failed_or_timed_out_mutation_is_preserved_without_replay(self):
        for reason in ["provider observation failed", "reboot-inject timed out"]:
            with self.subTest(reason=reason):
                case = SimpleNamespace(command=Mock(side_effect=RuntimeError(reason)), api=Mock(),
                                       local_state=lambda _: {"deploy": {"instance_id": "i-one", "public_ip": "192.0.2.1"}})
                with patch.object(inference_case, "instance_status", return_value="Running"), \
                        patch.object(inference_case, "boot_id", return_value="old-boot"), \
                        patch.object(inference_case, "changed_boot") as changed, \
                        patch.object(inference_case.time, "sleep"), \
                        patch.object(inference_case, "eventually", side_effect=self.poll), \
                        patch.object(inference_case, "validate_live") as live:
                    with self.assertRaisesRegex(RuntimeError, reason):
                        inference_case.reboot(case, "service", Path("spec.json"), "challenge", "i-one")
                case.command.assert_called_once()
                live.assert_not_called()
                changed.assert_not_called()

    def test_trustee_reboot_keeps_automatic_delivery(self):
        case = SimpleNamespace(command=Mock(), api=Mock(),
                               local_state=lambda _: {"deploy": {"instance_id": "i-one"}})
        with patch.object(inference_case, "instance_status", return_value="Running"), \
                patch.object(inference_case, "boot_id", return_value="old-boot"), \
                patch.object(inference_case, "changed_boot") as changed, \
                patch.object(inference_case.time, "sleep"), \
                patch.object(inference_case, "eventually", side_effect=self.poll), \
                patch.object(inference_case, "validate_live", return_value=[{}]):
            inference_case.reboot(case, "service", Path("spec.json"), "trustee", "i-one")
        case.command.assert_not_called()
        changed.assert_called_once_with(case, "service", "i-one", "old-boot", "reboot-i-one")


if __name__ == "__main__":
    unittest.main()
