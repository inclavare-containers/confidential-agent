import json
import os
from pathlib import Path
import runpy
import shlex
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fixtures.allinone import generate


class HarnessFixturesTest(unittest.TestCase):
    def test_generated_ports_match_the_image_runtime_for_every_harness(self):
        root = Path(__file__).resolve().parents[3]
        runtime = runpy.run_path(str(root / "examples/allinone/files/cai-agent"))
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                "DASHSCOPE_API_KEY": "fixture-key", "QODER_PERSONAL_ACCESS_TOKEN": "fixture-account",
                "E2E_QODER_MODEL": "fixed-model"}, clear=True):
            for mode in ["challenge", "trustee"]:
                fixtures = generate(Path(directory) / mode, {}, mode, "cn-beijing", mode)
                for fixture in fixtures:
                    with self.subTest(mode=mode, harness=fixture.name):
                        spec = json.loads((fixture / "appspec.json").read_text())
                        port = runtime["load_configuration"](fixture / "runtime.json")[2]
                        context = fixture / "application-runtime.json"
                        context.write_text(json.dumps({"schema_version": "confidential-agent/application-runtime/v1",
                                                       "ports": spec["service"]["ports"]}))
                        runtime["validate_declared_ports"](port, context)
                        if fixture.name == "openclaw":
                            self.assertEqual(spec["service"]["connect"], [])
                            # OpenClaw rejects --local when this fixture's gateway owns
                            # the state directory; model calls must use that gateway.
                            command = shlex.split(json.loads((fixture / "probe.json").read_text())["command"])
                            self.assertEqual(command[:2], ["cai-agent", "agent"])
                            self.assertNotIn("--local", command)
                            self.assertEqual(command[command.index("--agent") + 1], "main")
                            context.write_text(json.dumps({"schema_version": "confidential-agent/application-runtime/v1",
                                                           "ports": []}))
                            with self.assertRaisesRegex(ValueError, "listening port"):
                                runtime["validate_declared_ports"](port, context)

    def test_qoder_rejects_automatic_model_routing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, selected in enumerate(["", "auto", " auto "]):
                with self.subTest(selected=selected), patch.dict(os.environ, {"DASHSCOPE_API_KEY": "fixture-key",
                        "QODER_PERSONAL_ACCESS_TOKEN": "fixture-account", "E2E_QODER_MODEL": selected}, clear=True):
                    with self.assertRaisesRegex(RuntimeError, "fixed model"):
                        generate(root / str(index), {}, "challenge", "cn-beijing", "test", harnesses=["qodercli"])

    def test_qoder_selects_fixed_model_without_rewriting_byok_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            auth = root / "native"
            auth.mkdir()
            settings = {"model": {"name": "byok-configured-model"},
                        "credential": "opaque-qwen3.7-max-must-not-change"}
            (auth / "settings.json").write_text(json.dumps(settings))
            with patch.dict(os.environ, {"DASHSCOPE_API_KEY": "fixture-key",
                    "QODER_PERSONAL_ACCESS_TOKEN": "fixture-account", "E2E_QODER_NATIVE_CONFIG": str(auth)}, clear=True):
                [fixture] = generate(root / "fixtures", {}, "challenge", "cn-beijing", "test", harnesses=["qodercli"])
            self.assertIn("--model byok-configured-model", json.loads((fixture / "probe.json").read_text())["command"])
            self.assertEqual(json.loads((fixture / "qoder-settings.json").read_text())["credential"], settings["credential"])

    def test_platform_fixtures_do_not_require_an_unrelated_qoder_account(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.dict(os.environ, {"DASHSCOPE_API_KEY": "fixture-key"}, clear=True):
                fixtures = generate(Path(directory) / "fixtures", {}, "challenge", "cn-beijing", "test",
                                    harnesses=["codex", "opencode", "hermes"])
            self.assertEqual([fixture.name for fixture in fixtures], ["codex", "opencode", "hermes"])

    def test_qoder_migrates_legacy_context_before_binding_resource_digest(self):
        for preferences in [{}, {"native-model": {"contextWindow": 262144, "other": "kept"}}]:
            with self.subTest(preferences=preferences), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                auth = root / "native"
                auth.mkdir()
                settings = {"model": {"name": "native-model", "contextWindow": 131072,
                                      "preferences": preferences},
                            "credential": "opaque-test-credential"}
                original = json.dumps(settings)
                (auth / "settings.json").write_text(original)
                with patch.dict(os.environ, {"DASHSCOPE_API_KEY": "fixture-key",
                        "QODER_PERSONAL_ACCESS_TOKEN": "fixture-account",
                        "E2E_QODER_NATIVE_CONFIG": str(auth),
                        "E2E_QODER_MODEL": "explicit-model"}, clear=True):
                    [fixture] = generate(root / "fixtures", {}, "challenge", "cn-beijing", "test",
                                         harnesses=["qodercli"])
                generated = json.loads((fixture / "qoder-settings.json").read_text())
                self.assertNotIn("contextWindow", generated["model"])
                self.assertEqual(generated["model"]["preferences"]["native-model"],
                                 preferences.get("native-model", {"contextWindow": 131072}))
                self.assertEqual(generated["credential"], settings["credential"])
                self.assertEqual((auth / "settings.json").read_text(), original)
                self.assertIn("--model explicit-model", json.loads((fixture / "probe.json").read_text())["command"])
                spec = json.loads((fixture / "appspec.json").read_text())
                self.assertFalse(spec["resources"]["qoder-settings_json"]["mutable"])


if __name__ == "__main__":
    unittest.main()
