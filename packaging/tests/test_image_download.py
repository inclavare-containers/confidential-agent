import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('download_image', ROOT / 'hack/download-release-image.py')
download = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(download)

DATA = b'resumable official disk'
DISK = 'sha256:' + hashlib.sha256(DATA).hexdigest()
RELEASE = {'disk_digest': DISK, 'disk_size': len(DATA)}
REFERENCE = download.SOURCE['oci_repository'] + '@sha256:' + 'a' * 64


class ImageDownloadTests(unittest.TestCase):
    def test_mirror_resumes_disk_at_origin_offset(self):
        attempts = []

        def request(url, *args, token=None):
            host = url.split('/')[2]
            if url.endswith('/token'):
                self.assertIn('scope=repository:inclavare-containers/confidential-agent/allinone:pull', args)
                return 0, 200, '', b'{"token":"anonymous"}'
            if token is None:
                return 22, 401, f'www-authenticate: Bearer realm="https://{host}/token",service="{host}"', b''
            self.assertEqual(token, 'anonymous')
            self.assertEqual(args[args.index('--continue-at') + 1], '-')
            part = Path(args[args.index('--output') + 1])
            offset = part.stat().st_size if part.exists() else 0
            attempts.append((host, offset))
            if host == 'ghcr.io':
                if not offset:
                    part.write_bytes(DATA[:8])
                    return 18, 200, '', b''
                return 33, 200, '', b''
            with part.open('ab') as stream:
                stream.write(DATA[offset:])
            return 0, 206, '', b''

        manifest = {
            'artifactType': 'application/vnd.shelter.artifact.v1',
            'layers': [{'digest': DISK, 'size': len(DATA)}],
        }
        with (
            tempfile.TemporaryDirectory() as temp,
            patch.object(download, 'request', side_effect=request),
            patch.object(download.subprocess, 'check_output', return_value=json.dumps(manifest)),
            patch.object(download.subprocess, 'run') as oras,
        ):
            work = Path(temp)
            download.copy_layout(REFERENCE, RELEASE, work, 'oras')
            self.assertEqual(
                attempts, [('ghcr.io', 0), ('ghcr.io', 8), ('ghcr.1ms.run', 8)],
            )
            self.assertEqual((work / 'layout/blobs/sha256' / DISK[7:]).read_bytes(), DATA)
            self.assertFalse((work / 'disk.part').exists())
            self.assertEqual(oras.call_args.args[0][-2], REFERENCE.replace('ghcr.io', 'ghcr.1ms.run', 1))

    def test_integrity_error_fails_without_http_fallback(self):
        manifest = {
            'artifactType': 'application/vnd.shelter.artifact.v1',
            'layers': [{'digest': DISK, 'size': len(DATA)}],
        }
        with (
            tempfile.TemporaryDirectory() as temp,
            patch.object(download.subprocess, 'check_output', return_value=json.dumps(manifest)) as fetch,
            patch.object(download.subprocess, 'run') as oras,
        ):
            work = Path(temp)
            (work / 'disk.part').write_bytes(b'x' * len(DATA))
            with self.assertRaisesRegex(ValueError, 'does not match the release size and SHA-256'):
                download.copy_layout(REFERENCE, RELEASE, work, 'oras')
            fetch.assert_called_once()
            oras.assert_not_called()
            self.assertFalse((work / 'layout/blobs/sha256' / DISK[7:]).exists())


if __name__ == '__main__':
    unittest.main()
