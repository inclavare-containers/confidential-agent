"""A pre-reboot ready response must not satisfy cold-boot model recovery."""
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_case


class ModelKeyRecoveryTests(unittest.TestCase):
    def test_recovery_waits_for_a_cold_boot_and_preserves_start_failure(self):
        for fail_start in [False, True]:
            with self.subTest(fail_start=fail_start), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                key = root / "model-key"
                key.write_bytes(b"original-model-key\n")
                spec = root / "spec.json"
                spec.write_text(json.dumps({"resources": {
                    "model_decryption_key": {"source": str(key)}}}))
                events = []
                cloud = {"status": "Running", "stop_reads": 0, "new_boot": False,
                         "wrong_boot": False, "wrong_boot_reads": 0}

                def command(label, *args, **kwargs):
                    if label == "wrong-key-delivery":
                        self.assertNotEqual(key.read_bytes(), b"original-model-key\n")
                    elif label == "wrong-key-status":
                        events.append("wrong-key-not-ready")
                        return SimpleNamespace(stdout=b'[{"daemon":{"app_ready":false}}]')
                    elif label == "restore-key-delivery":
                        self.assertEqual(key.read_bytes(), b"original-model-key\n")
                        events.append("restored-old-boot-ready")
                    return SimpleNamespace(stdout=b"")

                def api(product, action, **parameters):
                    self.assertEqual(product, "ecs")
                    if action == "DescribeInstances":
                        if cloud["status"] == "Stopping":
                            cloud["stop_reads"] += 1
                            if cloud["stop_reads"] == 2:
                                cloud["status"] = "Stopped"
                        return {"Instances": {"Instance": [{"Status": cloud["status"]}]}}
                    if action == "StopInstance":
                        cloud["status"] = "Stopping"
                    elif action == "StartInstance":
                        self.assertEqual(cloud["status"], "Stopped")
                        if fail_start:
                            raise RuntimeError("original start failure")
                        cloud.update(status="Running", new_boot=True)
                    else:
                        self.assertEqual(action, "RebootInstance")
                        cloud["wrong_boot"] = True
                        # Reboot acknowledgement can precede shutdown. The old
                        # daemon is still ready after the restored-key delivery.
                    events.append(action)
                    return {}

                def ready(*args):
                    self.assertTrue(cloud["new_boot"], "accepted readiness from the old boot")
                    events.append("new-boot-ready")
                    return [{}]

                def request(base, path, body=None):
                    if path == "/v1/models":
                        events.append("wrong-key-request-denied")
                        raise urllib.error.URLError("encrypted model unavailable")
                    self.assertTrue(cloud["new_boot"])
                    events.append("real-model-response")
                    return json.dumps({"model": inference_case.MODEL, "choices": [
                        {"message": {"content": "INFERENCE_OK"}}]}).encode()

                def ssh(*args, **kwargs):
                    number = 3 if cloud["new_boot"] else 2 if cloud["wrong_boot"] else 1
                    if cloud["wrong_boot"] and not cloud["new_boot"]:
                        cloud["wrong_boot_reads"] += 1
                        # The old daemon can still answer immediately after the
                        # cloud accepts a reboot. Do not accept that boot ID.
                        if cloud["wrong_boot_reads"] == 1:
                            number = 1
                    return SimpleNamespace(stdout=("00000000-0000-0000-0000-00000000000" + str(number)).encode())

                case = SimpleNamespace(command=command, api=api, ssh=ssh, checks={}, work=root,
                    local_state=lambda _: {"deploy": {"public_ip": "192.0.2.1", "instance_id": "i-owned"}})
                with patch.object(inference_case, "validate_live", side_effect=ready), \
                        patch.object(inference_case, "request", side_effect=request), \
                        patch.object(inference_case.time, "sleep"), \
                        patch.object(inference_case, "eventually", side_effect=self.poll):
                    if fail_start:
                        with self.assertRaisesRegex(RuntimeError, "original start failure"):
                            inference_case.wrong_model_key(case, "service", spec, "i-owned", "http://localhost")
                        self.assertNotIn("wrong_model_key_rejected", case.checks)
                        self.assertNotIn("real-model-response", events)
                    else:
                        inference_case.wrong_model_key(case, "service", spec, "i-owned", "http://localhost")
                        self.assertTrue(case.checks["wrong_model_key_rejected"])
                        self.assertLess(events.index("StartInstance"), events.index("new-boot-ready"))
                        self.assertEqual(events.count("real-model-response"), 1)
                        wrong_proof = json.loads((root / "wrong-key-boot-proof.json").read_text())
                        restored_proof = json.loads((root / "restored-key-boot-proof.json").read_text())
                        self.assertNotEqual(wrong_proof["before"], wrong_proof["after"])
                        self.assertEqual(restored_proof["before"], wrong_proof["after"])
                        self.assertNotEqual(restored_proof["before"], restored_proof["after"])
                self.assertEqual(key.read_bytes(), b"original-model-key\n")
                self.assertEqual(events.count("wrong-key-not-ready"), 8)
                self.assertEqual(events.count("wrong-key-request-denied"), 8)
                self.assertGreaterEqual(cloud["wrong_boot_reads"], 2)

    @staticmethod
    def poll(probe, timeout):
        for _ in range(5):
            if probe():
                return
        raise AssertionError("readiness did not converge")


if __name__ == "__main__":
    unittest.main()
