"""Exercise the live publication reference probe's command sequence."""
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
    def probe(self, public_export=False):
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
            if public_export:
                original["build"]["published"]["release"].pop("import_task_id")
                original["build"]["published"]["release"].pop("import_snapshot_ids")
            publication.write_text(json.dumps(original))
            calls = []

            def command(label, *args, **options):
                calls.append(label)
                if label == "cleanup-reject-consumers":
                    self.assertNotIn("--force", args)
                    self.assertNotIn("env", options)
                    return subprocess.CompletedProcess([], 1, b"", b"image is retained: consumer")
                self.assertEqual(label, "cleanup-prune-retains-consumers")
                return subprocess.CompletedProcess([], 0, b"no unreferenced images\n", b"")

            consumer = SimpleNamespace(directory=service, state=root / "state", work=root,
                                       service="consumer", spec={"deploy": {"region": "test-region"}},
                                       command=command)
            cloud = Mock(return_value=subprocess.CompletedProcess([], 0,
                json.dumps({"Images": {"Image": [{"ImageId": "m-shared"}]}}).encode(), b""))
            with patch.object(reuse, "run", cloud):
                reuse.verify_publication_cleanup([consumer], publication, "aliyun")
            self.assertEqual(calls, ["cleanup-reject-consumers", "cleanup-prune-retains-consumers"])
            self.assertEqual(json.loads(publication.read_text()), original)
            cloud.assert_called_once()
            self.assertIn("DescribeImages", cloud.call_args.args[0])
            self.assertIn("m-shared", cloud.call_args.args[0])

    def test_live_reference_prevents_retirement(self):
        self.probe()

    def test_public_export_keeps_schema_default_empty_snapshot_custody(self):
        self.probe(public_export=True)


if __name__ == "__main__":
    unittest.main()
