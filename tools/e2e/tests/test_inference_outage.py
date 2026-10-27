"""A published debug key cannot unlock a cold boot while Trustee is down."""
import base64
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_case
from cloud_case import Case


class InferenceOutageTests(unittest.TestCase):
    @staticmethod
    def poll(probe, timeout):
        for _ in range(3):
            if probe():
                return
        raise RuntimeError("readiness did not converge")

    def test_cold_boot_failure_evidence_and_automatic_recovery(self):
        for failure in [None, "ssh-unlocked", "model-serves", "missing-console", "start-fails"]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                cloud = {"status": "Running", "recovered": False}

                def api(product, action, **parameters):
                    if action == "DescribeInstances":
                        return {"Instances": {"Instance": [{"Status": cloud["status"]}]}}
                    if action == "GetInstanceConsoleOutput":
                        console = b"ordinary startup" if failure == "missing-console" else b"Trustee key release failed"
                        return {"ConsoleOutput": base64.b64encode(console).decode()}
                    if action == "StopInstance":
                        cloud["status"] = "Stopped"
                    elif action == "StartInstance":
                        self.assertEqual(cloud["status"], "Stopped")
                        if failure == "start-fails":
                            raise RuntimeError("original start failure")
                        cloud.update(status="Running", recovered=True)
                    return {}

                def ssh(service, label, command, **options):
                    self.assertEqual(options["identity_file"], root / "original-debug-key")
                    self.assertIsNone(options["instance_id"])
                    if command == "true":
                        return SimpleNamespace(returncode=0 if failure == "ssh-unlocked" else 255, stdout=b"")
                    number = "2" if cloud["recovered"] else "1"
                    return SimpleNamespace(returncode=0, stdout=("00000000-0000-0000-0000-00000000000" + number).encode())

                case = SimpleNamespace(work=root, api=Mock(side_effect=api), ssh=Mock(side_effect=ssh),
                    local_state=lambda _: {"deploy": {"instance_id": "i-beta"}},
                    command=Mock(return_value=SimpleNamespace(stdout=b'[{"live_error":"unreachable","daemon":{"app_ready":false}}]')),
                    trustee_action=Mock(), checks={}, debug_identity=root / "original-debug-key")
                request = Mock(return_value=b'{"data":[]}') if failure == "model-serves" else Mock(side_effect=urllib.error.URLError("unavailable"))
                with patch.object(inference_case.time, "sleep"), \
                        patch.object(inference_case, "eventually", side_effect=self.poll), \
                        patch.object(inference_case, "request", request), \
                        patch.object(inference_case, "infer") as infer, \
                        patch.object(inference_case, "validate_live", return_value=[{}]) as live:
                    if failure:
                        with self.assertRaises((AssertionError, RuntimeError)):
                            inference_case.trustee_outage(case, "beta", root / "spec.json", "i-beta", "beta-front", warm_front="group-front")
                        self.assertNotIn("trustee_cold_boot_fail_closed", case.checks)
                        live.assert_not_called()
                    else:
                        inference_case.trustee_outage(case, "beta", root / "spec.json", "i-beta", "beta-front", warm_front="group-front")
                        self.assertTrue(case.checks["trustee_cold_boot_fail_closed"])
                        self.assertEqual(infer.call_count, 9)
                        self.assertEqual(infer.call_args.args, ("beta-front",))
                        proof = json.loads((root / "trustee-outage-boot-proof.json").read_text())
                        self.assertNotEqual(proof["before"], proof["after"])
                self.assertEqual([call.args[0] for call in case.trustee_action.call_args_list], ["stop", "start"])
                self.assertFalse(any(call.args[1:2] == ("inject",) for call in case.command.call_args_list))

    def test_case_ssh_passes_original_key_and_replica_to_cli(self):
        case = Case.__new__(Case)
        case.work = Path("/consumer")
        case.command = Mock()
        case.ssh("consumer", "diagnostic", "true", instance_id="i-owned", identity_file=Path("/producer/original-key"))
        arguments = case.command.call_args.args
        self.assertEqual(arguments[:7], ("diagnostic", "ssh", "consumer", "--instance-id", "i-owned", "--identity-file", Path("/producer/original-key")))

    def test_boot_probe_selects_recorded_replicas_and_single_instance(self):
        for deployment, target, selector in [
            ({"instance_ids": ["i-first", "i-second"]}, "i-second", "i-second"),
            ({"instance_id": "i-single"}, "i-single", None),
            ({"instance_ids": [], "instance_id": "i-single"}, "i-single", None),
        ]:
            with self.subTest(deployment=deployment):
                case = SimpleNamespace(local_state=lambda _: {"deploy": deployment},
                    ssh=Mock(return_value=SimpleNamespace(stdout=b"00000000-0000-0000-0000-000000000001")),
                    debug_identity=Path("/producer/original-key"))
                self.assertEqual(inference_case.boot_id(case, "service", target, "boot"),
                                 "00000000-0000-0000-0000-000000000001")
                self.assertEqual(case.ssh.call_args.kwargs["instance_id"], selector)
                self.assertEqual(case.ssh.call_args.kwargs["identity_file"], Path("/producer/original-key"))

    def test_boot_probe_rejects_targets_absent_from_recorded_state(self):
        for deployment in [
            {"instance_ids": ["i-known"]},
            {"instance_ids": ["i-known"], "instance_id": "i-unknown"},
            {"instance_id": "i-known"},
            {},
        ]:
            with self.subTest(deployment=deployment):
                case = SimpleNamespace(local_state=lambda _: {"deploy": deployment}, ssh=Mock())
                with self.assertRaisesRegex(AssertionError, "recorded"):
                    inference_case.boot_id(case, "service", "i-unknown", "boot")
                case.ssh.assert_not_called()


if __name__ == "__main__":
    unittest.main()
