"""The one-click installer forwards installation options to the RPM installer."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BootstrapTests(unittest.TestCase):
    def test_checkout_entrypoint_delegates_without_installing_or_deploying(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'one-click').mkdir()
            (root / 'packaging').mkdir()
            entry = root / 'one-click/install.sh'
            entry.write_text((ROOT / 'one-click/install.sh').read_text())
            (root / 'packaging/install-rpm.sh').write_text('printf "%s\\n" "$@"\n')
            result = subprocess.run(['sh', str(entry), '--from-dir', '/release files', '--download-only',
                                     '--output-dir', '/offline'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['--from-dir', '/release files', '--download-only',
                                                         '--output-dir', '/offline'])


if __name__ == '__main__':
    unittest.main()
