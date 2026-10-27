"""Require native NVIDIA CC mode queries and reject insecure GPU states."""
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from inference_case import validate_debug_gpu


class GpuDebugDiagnosticTests(unittest.TestCase):
    def test_native_queries_require_confidential_mode_without_devtools(self):
        for cc, devtools, version, valid in [
            (b"CC status: ON\n", b"DevTools Mode: OFF\n", b"0.28.0\n", True),
            (b" CC status : ON\n", b"DevTools mode : OFF\n", b"0.28.0\n", True),
            (b"CC status: OFF\n", b"DevTools Mode: OFF\n", b"0.28.0\n", False),
            (b"CC status: ON\n", b"DevTools Mode: ON\n", b"0.28.0\n", False),
            (b"", b"DevTools Mode: OFF\n", b"0.28.0\n", False),
            (b"CC status: N/A\n", b"DevTools Mode: OFF\n", b"0.28.0\n", False),
            (b"unrelated On\n", b"DevTools Mode: OFF\n", b"0.28.0\n", False),
            (b"CC status: ON\n", b"DevTools Mode: OFF\n", b"0.27.0\n", False),
        ]:
            with self.subTest(cc=cc, devtools=devtools, version=version):
                case = Mock()
                case.local_state.return_value = {"deploy": {"instance_ids": ["i-owned"]}}
                case.ssh.side_effect = [subprocess.CompletedProcess([], 0, value, b"")
                                        for value in [cc, devtools, version]]
                if valid:
                    validate_debug_gpu(case, "inference-debug", instance_id="i-owned")
                    self.assertEqual([call.args[2] for call in case.ssh.call_args_list],
                                     ["nvidia-smi conf-compute -f", "nvidia-smi conf-compute -d",
                                      "/root/.venv/bin/vllm --version"])
                    self.assertTrue(all(call.kwargs['instance_id'] == "i-owned"
                                        for call in case.ssh.call_args_list))
                else:
                    with self.assertRaises(AssertionError):
                        validate_debug_gpu(case, "inference-debug", instance_id="i-owned")


if __name__ == '__main__':
    unittest.main()
