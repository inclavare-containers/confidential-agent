"""Interrupt only after the candidate Shelter successfully creates a release VM."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
spec = importlib.util.spec_from_file_location(
    "provider_interruption", Path(__file__).resolve().parents[1] / "allinone-reuse.py"
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ProviderInterruptionTest(unittest.TestCase):
    def test_marks_only_successful_release_run_and_preserves_provider_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real, sleep, wrapper = (root / name for name in ['shelter', 'sleep', 'wrapper'])
            marker = root / 'completed'
            real.write_text('#!/bin/sh\nexit "${TEST_PROVIDER_STATUS:-0}"\n')
            sleep.write_text('#!/bin/sh\nexit 0\n')
            real.chmod(0o700)
            sleep.chmod(0o700)
            module.write_interruption_wrapper(wrapper, str(real), marker)
            environment = dict(os.environ, PATH=str(root) + os.pathsep + os.defpath)
            for arguments, code, marked in [
                (['image', 'inspect-release', 'release.json'], 0, False),
                (['run', '-d', '--release', 'release.json', '-c', 'runtime.yaml'], 42, False),
                (['run', '-d', '--release', 'release.json', '-c', 'runtime.yaml'], 0, True),
            ]:
                with self.subTest(arguments=arguments, code=code):
                    process = subprocess.run([str(wrapper), *arguments],
                                             env=dict(environment, TEST_PROVIDER_STATUS=str(code)),
                                             capture_output=True, timeout=5)
                    self.assertEqual(process.returncode, code)
                    self.assertEqual(marker.exists(), marked)
