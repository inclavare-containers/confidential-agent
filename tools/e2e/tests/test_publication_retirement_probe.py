"""Keep denied-retirement probes aligned with durable retry custody."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

E2E = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(E2E))
spec = importlib.util.spec_from_file_location("allinone_reuse", E2E / "allinone-reuse.py")
reuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reuse)


class PublicationRetirementProbeTest(unittest.TestCase):
    def probe(self, mutation=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            service = root / "state/services/consumer"
            service.mkdir(parents=True)
            (service / "manifest.json").write_text(json.dumps({"image": {"image_id": "m-shared"}}))
            publication = root / "publication.json"
            original = {"service_id": "publisher", "phase": "built", "deploy": {"published_image_id": "m-shared"},
                        "build": {"published": {"release": {"status": "available", "retiring": False,
                            "image_id": "m-shared", "owned": True, "exported": True, "oss_cleaned": False,
                            "import_task_id": "task-owned", "import_snapshot_ids": ["snapshot-owned"]}}}}
            publication.write_text(json.dumps(original))
            calls = []

            def command(label, *args, **options):
                calls.append(label)
                if label == "cleanup-reject-consumers":
                    self.assertEqual(options["env"]["ALICLOUD_ACCESS_KEY"], "CA_E2E_INVALID_ACCESS_KEY")
                    self.assertNotIn("ALICLOUD_STS_TOKEN", options["env"])
                    return subprocess.CompletedProcess([], 1, b"", b"image is retained: consumer")
                if label == "cleanup-prune-retains-consumers":
                    return subprocess.CompletedProcess([], 0, b"no unreferenced images\n", b"")
                self.assertEqual(label, "cleanup-denied-cloud-delete")
                self.assertIn("--force", args)
                state_file = root / "state/services/reuse-test-publisher/state.json"
                state = json.loads(state_file.read_text())
                # Native retirement saves intent before the first cloud call;
                # credential rejection leaves availability and resource IDs intact.
                state["build"]["published"]["release"]["retiring"] = True
                if mutation:
                    mutation(state["build"]["published"]["release"])
                state_file.write_text(json.dumps(state))
                return subprocess.CompletedProcess([], 1, b"", b"image unpublish partially failed")

            consumer = SimpleNamespace(directory=service, state=root / "state", work=root,
                                       service="consumer", spec={"deploy": {"region": "test-region"}},
                                       command=command)
            cloud = Mock(return_value=subprocess.CompletedProcess([], 0,
                json.dumps({"Images": {"Image": [{"ImageId": "m-shared"}]}}).encode(), b""))
            with patch.object(reuse, "run", cloud), patch.dict("os.environ", {"ALICLOUD_STS_TOKEN": "not-sent"}):
                reuse.verify_publication_cleanup([consumer], publication, "aliyun")
            self.assertEqual(calls, ["cleanup-reject-consumers", "cleanup-prune-retains-consumers",
                                     "cleanup-denied-cloud-delete"])
            self.assertEqual(json.loads(publication.read_text()), original)
            cloud.assert_called_once()
            self.assertIn("DescribeImages", cloud.call_args.args[0])
            self.assertIn("m-shared", cloud.call_args.args[0])

    def test_failed_retirement_keeps_available_image_and_retry_custody(self):
        self.probe()

    def test_failed_retirement_must_not_lose_intent_or_resource_identity(self):
        changes = {"retiring": False, "status": "failed", "image_id": None, "owned": False,
                   "oss_cleaned": False, "import_task_id": None, "import_snapshot_ids": []}
        for field, value in changes.items():
            with self.subTest(field=field), self.assertRaises(AssertionError):
                self.probe(lambda entry: entry.update({field: value}))


if __name__ == "__main__":
    unittest.main()
