"""Revocation must converge on the original survivor before real inference."""
import copy
import io
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


class SurvivingConsumerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.before = {"phase": "active", "mesh_generation": 2, "gateway_identity": "beta-public-key",
                       "deploy": {"instance_id": "i-beta", "public_ip": "192.0.2.3", "run_id": "run-beta",
                                  "resource_name": "beta", "published_image_id": "m-shared"}}
        self.current = copy.deepcopy(self.before)
        self.current["mesh_generation"] = 3
        self.clock = 0
        self.case = SimpleNamespace(work=self.root, local_state=lambda _: copy.deepcopy(self.current),
                                    command=Mock(side_effect=lambda *args, **kw: self.status()))
        self.reply = json.dumps({"model": inference_case.MODEL,
                                "choices": [{"message": {"content": "INFERENCE_OK"}}]}).encode()

    def view(self):
        return {"local": {"service_id": "beta", "phase": "active", "mesh_generation": 3,
                          "cloud": copy.deepcopy(self.before["deploy"])},
                "replicas": [{"instance_id": "i-beta", "daemon": {"service_id": "beta", "phase": "running",
                    "mesh_generation": 3, "app_ready": True, "mesh_ready": True}}]}

    def status(self, value=None):
        return SimpleNamespace(stdout=json.dumps([self.view() if value is None else value]).encode())

    def sleep(self, seconds):
        self.clock += seconds

    def verify(self, timeout=10):
        with patch.object(inference_case.time, "monotonic", side_effect=lambda: self.clock), \
                patch.object(inference_case.time, "sleep", side_effect=self.sleep):
            inference_case.verify_surviving_consumer(self.case, "beta", self.before, "http://same-tunnel", timeout=timeout)

    def proof(self):
        return json.loads((self.root / "survivor-after-destroy.json").read_text())

    @staticmethod
    def http_error(code=500, body=b'{"code":"ShouldRequestNewKeyConfigFromServerError"}'):
        return urllib.error.HTTPError("http://same-tunnel", code, "transport error", {}, io.BytesIO(body))

    def test_requires_joint_readiness_at_new_committed_generation_before_real_inference(self):
        stale, app_pending, mesh_pending, ready = [self.view() for _ in range(4)]
        stale["replicas"][0]["daemon"]["mesh_generation"] = 2
        app_pending["replicas"][0]["daemon"]["app_ready"] = False
        mesh_pending["replicas"][0]["daemon"]["mesh_ready"] = False
        self.case.command.side_effect = [self.status(view) for view in (stale, app_pending, mesh_pending, ready)]
        with patch.object(inference_case, "request", return_value=self.reply) as request:
            self.verify(timeout=30)
        self.assertEqual(self.case.command.call_count, 4)
        request.assert_called_once()
        self.assertEqual(request.call_args.args[:2], ("http://same-tunnel", "/v1/chat/completions"))
        self.assertLessEqual(request.call_args.kwargs["timeout"], 15)
        self.assertTrue(self.proof()["complete"])
        self.assertEqual((self.root / "survivor-model-response.json").read_bytes(), self.reply)

    def test_stale_missing_and_permanently_unready_generations_never_call_the_model(self):
        for field, value in (("mesh_generation", 2), ("mesh_generation", None), ("mesh_generation", 4),
                             ("app_ready", False), ("mesh_ready", False), ("phase", "starting-mesh")):
            with self.subTest(field=field, value=value):
                self.clock = 0
                view = self.view()
                view["replicas"][0]["daemon"][field] = value
                self.case.command.side_effect = lambda *args, **kw: self.status(view)
                with patch.object(inference_case, "infer") as infer, self.assertRaisesRegex(RuntimeError, "readiness budget"):
                    self.verify()
                infer.assert_not_called()
                self.assertFalse(self.proof()["complete"])

    def test_local_generation_must_advance_after_primary_deletion(self):
        self.current["mesh_generation"] = 2
        view = self.view()
        view["local"]["mesh_generation"] = view["replicas"][0]["daemon"]["mesh_generation"] = 2
        self.case.command.side_effect = lambda *args, **kw: self.status(view)
        with patch.object(inference_case, "infer") as infer, self.assertRaises(RuntimeError):
            self.verify()
        infer.assert_not_called()

    def test_changed_instance_deployment_gateway_or_daemon_identity_fails_immediately(self):
        for change in ("recorded-instance", "recorded-image", "gateway", "live-instance", "replica", "daemon", "service"):
            with self.subTest(change=change):
                self.current = copy.deepcopy(self.before)
                self.current["mesh_generation"] = 3
                view = self.view()
                if change == "recorded-instance": self.current["deploy"]["instance_id"] = "i-other"
                if change == "recorded-image": self.current["deploy"]["published_image_id"] = "m-other"
                if change == "gateway": self.current["gateway_identity"] = "other-public-key"
                if change == "live-instance": view["local"]["cloud"]["instance_id"] = "i-other"
                if change == "replica": view["replicas"][0]["instance_id"] = "i-other"
                if change == "daemon": view["replicas"][0]["daemon"]["service_id"] = "other"
                if change == "service": view["local"]["service_id"] = "other"
                self.case.command.side_effect = lambda *args, **kw: self.status(view)
                with patch.object(inference_case, "infer") as infer, self.assertRaises(AssertionError):
                    self.verify()
                infer.assert_not_called()
                self.assertFalse(self.proof()["complete"])

    def test_known_key_refresh_retries_on_same_tunnel_and_still_requires_real_model_output(self):
        with patch.object(inference_case, "request", side_effect=[self.http_error(), self.reply]) as request:
            self.verify()
        self.assertEqual(request.call_count, 2)
        self.assertEqual({call.args[0] for call in request.call_args_list}, {"http://same-tunnel"})
        self.assertEqual(self.proof()["attempts"][0]["ohttp_error_code"], "ShouldRequestNewKeyConfigFromServerError")
        self.assertTrue(self.proof()["complete"])

    def test_unknown_http_errors_and_incorrect_model_fail_without_retry(self):
        for response in (self.http_error(body=b'{"code":"other"}'), self.http_error(code=403),
                         self.http_error(body=b'[]'), self.http_error(body=b'not json'),
                         b'{"model":"wrong","choices":[{"message":{"content":"INFERENCE_OK"}}]}',
                         json.dumps({"model": inference_case.MODEL, "choices": [{"message": {"content": "wrong marker"}}]}).encode()):
            with self.subTest(response=type(response).__name__):
                request = Mock(side_effect=response) if isinstance(response, Exception) else Mock(return_value=response)
                with patch.object(inference_case, "request", request), self.assertRaises((urllib.error.HTTPError, AssertionError)):
                    self.verify()
                request.assert_called_once()
                self.assertFalse(self.proof()["complete"])

    def test_permanent_key_refresh_is_bounded_and_leaves_diagnostics(self):
        with patch.object(inference_case, "request", side_effect=lambda *args, **kw: (_ for _ in ()).throw(self.http_error())) as request:
            with self.assertRaisesRegex(RuntimeError, "readiness budget"):
                self.verify()
        self.assertEqual(request.call_count, 2)
        self.assertEqual(self.clock, 10)
        self.assertFalse(self.proof()["complete"])
        self.assertEqual(len(self.proof()["attempts"]), 2)


if __name__ == "__main__":
    unittest.main()
