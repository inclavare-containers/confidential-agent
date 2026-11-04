"""Only the pinned, upstream stripped Python may replace the full archive."""
import copy
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
INSTALLER = ROOT / 'examples/hermes-agent/files/install-hermes-agent-runtime.sh'
BASE = 'https://github.com/astral-sh/python-build-standalone/releases/download/20260901/'
NAME = 'cpython-3.14.7+20260901-x86_64-unknown-linux-gnu-install_only'
SOURCE = {'url': BASE + NAME + '.tar.gz',
          'sha256': '0ab3305457051cd3e7c031857e005f1bda17c218a1990567dacaaac6dd1d14f0'}
SELECTED = {'url': BASE + NAME + '_stripped.tar.gz',
            'sha256': '3959f92825141e04adf44982d3a83ee57af0877e893b0796e04c1468749d9b04'}


@unittest.skipUnless(sys.platform == 'linux' and platform.machine() == 'x86_64',
                     'the compact Python artifact targets Linux x86_64')
class CompactPythonTests(unittest.TestCase):
    def run_adapter(self, root, enabled='1'):
        function = re.search(r'(?ms)^prepare_hermes_stripped_python\(\) \{\n.*?^PY\n\}',
                             INSTALLER.read_text()).group()
        return subprocess.run(['bash', '-c', function + '\nprepare_hermes_stripped_python'],
                              env=dict(os.environ, HERMES_COMPACT_RUNTIME=enabled,
                                       HERMES_INSTALL_DIR=str(root / 'repo'),
                                       HERMES_PAYLOAD_DIR=str(root / 'payload')),
                              capture_output=True, text=True)

    def fixture(self, root):
        lock = {'packages': {'python': {'version': '3.14.7+20260901', 'artifacts': {
            'linux-x64': copy.deepcopy(SOURCE), 'linux-arm64': {'keep': 'unchanged'}}},
            'node': {'version': '26.7.0', 'keep': 'unchanged'}}, 'schema': 1}
        path = root / 'repo/pm/lock.json'
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps(lock))
        return path, lock

    def test_replacement_preserves_every_other_pin_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, expected = self.fixture(root)
            result = self.run_adapter(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            expected['packages']['python']['artifacts']['linux-x64'] = SELECTED
            self.assertEqual(json.loads(path.read_text()), expected)
            receipt = (root / 'payload/compact-python.json').read_bytes()
            after = path.read_bytes()
            self.assertEqual(self.run_adapter(root).returncode, 0)
            self.assertEqual(path.read_bytes(), after)
            self.assertEqual((root / 'payload/compact-python.json').read_bytes(), receipt)

    def test_unknown_version_or_archive_fails_without_rewriting(self):
        for field in ['version', 'sha256', 'url']:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path, lock = self.fixture(root)
                python = lock['packages']['python']
                (python if field == 'version' else python['artifacts']['linux-x64'])[field] = 'unexpected'
                path.write_text(json.dumps(lock))
                before = path.read_bytes()
                self.assertNotEqual(self.run_adapter(root).returncode, 0)
                self.assertEqual(path.read_bytes(), before)
                self.assertFalse((root / 'payload/compact-python.json').exists())

    def test_standalone_installer_keeps_its_original_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, _ = self.fixture(root)
            before = path.read_bytes()
            self.assertEqual(self.run_adapter(root, enabled='0').returncode, 0)
            self.assertEqual(path.read_bytes(), before)
            self.assertFalse((root / 'payload').exists())


if __name__ == '__main__':
    unittest.main()
