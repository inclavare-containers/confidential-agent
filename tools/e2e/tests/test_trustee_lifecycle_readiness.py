"""Trustee readiness must hold in one live snapshot at the local generation."""
import copy
import json
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


FLOW = Path(__file__).resolve().parents[1] / "cases/trustee-duo/flow.sh"


def service(name, phase="active"):
    return {"local": {"service_id": name, "phase": phase, "mesh_generation": 4},
            "daemon": {"phase": "running", "app_ready": True,
                       "mesh_ready": True, "mesh_generation": 4}, "live_error": None}


class TrusteeLifecycleReadinessTests(unittest.TestCase):
    def probe(self, snapshots, *, active="codex,openclaw", deleted="", capture_status=0):
        source = FLOW.read_text()
        functions = "\n".join(re.search(
            rf"(?ms)^{name}\(\) \{{\n.*?^\}}$", source).group()
            for name in ["assert_local_lifecycle_status", "wait_for_local_lifecycle_status"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, snapshot in enumerate(snapshots, 1):
                (root / f"fixture-{index}.json").write_text(json.dumps(snapshot))
            script = r'''
set -euo pipefail
WORK_DIR=$1
STATE_DIR=$1
active=$2
deleted=$3
capture_status=$4
snapshots=$5
calls=0
record_file_as_block() { :; }
sleep() { SECONDS=$((SECONDS + $1)); }
ca_capture() {
  calls=$((calls + 1))
  local fixture=$calls
  ((fixture <= snapshots)) || fixture=$snapshots
  cp "$WORK_DIR/fixture-$fixture.json" "$2"
  : > "$3"
  return "$capture_status"
}
'''
            script += functions + r'''
if wait_for_local_lifecycle_status "$active" "$deleted" lifecycle 10; then
  result=0
else
  result=$?
fi
printf '%s\n' "$calls" > "$WORK_DIR/calls"
exit "$result"
'''
            result = subprocess.run(["bash", "-c", script, "test", directory,
                                     active, deleted, str(capture_status), str(len(snapshots))],
                                    capture_output=True, text=True, timeout=10)
            calls = int((root / "calls").read_text())
            evidence = [json.loads(p.read_text()) for p in sorted(root.glob("status-*.json"))]
            return result, calls, evidence

    def test_waits_for_joint_ready_snapshot_and_preserves_pending_snapshot(self):
        ready = [service("codex"), service("openclaw")]
        pending = copy.deepcopy(ready)
        pending[0]["daemon"].update(phase="resources-applied", mesh_ready=False)
        result, calls, evidence = self.probe([pending, ready])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, 2)
        self.assertEqual(evidence, [pending, ready])

    def test_readiness_observed_separately_does_not_pass(self):
        first = [service("codex"), service("openclaw")]
        first[0]["daemon"]["mesh_ready"] = False
        second = [service("codex"), service("openclaw")]
        second[1]["daemon"]["app_ready"] = False
        result, calls, _ = self.probe([first, second])
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, 2)

    def test_stale_or_missing_generation_and_live_error_fail(self):
        for change in ["stale", "missing", "live-error"]:
            with self.subTest(change=change):
                items = [service("codex"), service("openclaw")]
                if change == "stale":
                    items[0]["daemon"]["mesh_generation"] = 3
                elif change == "missing":
                    del items[0]["local"]["mesh_generation"]
                else:
                    items[0]["live_error"] = "provider status unavailable"
                self.assertNotEqual(self.probe([items])[0].returncode, 0)

    def test_failed_capture_cannot_pass_from_valid_json(self):
        result, calls, _ = self.probe([[service("codex"), service("openclaw")]], capture_status=23)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, 2)

    def test_deleted_survivor_requires_current_active_generation(self):
        items = [service("codex", "deleted"), service("openclaw")]
        items[0]["daemon"] = None
        items[0]["live_error"] = "service is not active or deployed"
        self.assertEqual(self.probe([items], active="openclaw", deleted="codex")[0].returncode, 0)
        items[0]["local"]["phase"] = "active"
        self.assertNotEqual(self.probe([items], active="openclaw", deleted="codex")[0].returncode, 0)

    def test_missing_unknown_and_duplicate_services_fail(self):
        for items in [[service("codex")], [service("codex"), service("other")],
                      [service("codex"), service("openclaw"), service("codex")]]:
            with self.subTest(items=items):
                self.assertNotEqual(self.probe([items])[0].returncode, 0)


if __name__ == "__main__":
    unittest.main()
