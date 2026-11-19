#!/usr/bin/env python3
"""Resolve the exact companion RPM from a verified cache or public GHCR blob."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def verify(path, manifest):
    if digest(path) != manifest['sha256']:
        raise ValueError('Shelter RPM SHA256 mismatch: ' + str(path))
    payload = subprocess.check_output(
        ['rpm', '-qp', '--qf', '%{PAYLOADDIGEST}', str(path)], text=True).strip()
    if payload != manifest['payload_sha256']:
        raise ValueError('Shelter RPM payload digest mismatch: ' + str(path))


def resolve(manifest_path, cache):
    manifest = json.loads(manifest_path.read_text())
    name = manifest['file']
    if not isinstance(name, str) or not re.fullmatch(r'shelter-[A-Za-z0-9._+-]+\.rpm', name):
        raise ValueError('Invalid Shelter package filename')
    for key in ['sha256', 'payload_sha256']:
        if not re.fullmatch(r'[0-9a-f]{64}', manifest[key]):
            raise ValueError('Invalid ' + key)
    # Existing checkouts may still contain the formerly tracked RPM.
    legacy = manifest_path.parent / name
    target = cache / manifest['sha256'] / name
    for path in [legacy, target]:
        if path.is_file():
            verify(path, manifest)
            return path.resolve()
    reference = f"{manifest['oci_repository']}@sha256:{manifest['sha256']}"
    fetch_tools = Path(__file__).resolve().parents[1] / 'packaging/fetch-native-tools.sh'
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.fetch-', dir=target.parent) as temp:
        work = Path(temp)
        partial = work / name
        config = work / 'anonymous.json'
        config.write_text('{"auths":{}}')
        subprocess.run([fetch_tools, work, '', 'oras'], check=True, stdout=sys.stderr)
        subprocess.run([work / 'oras', 'blob', 'fetch', '--registry-config', config,
                        '--output', partial, reference], check=True, stdout=sys.stderr)
        verify(partial, manifest)
        os.replace(partial, target)
    return target.resolve()


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=root / 'hack/shelter-rpm.json')
    parser.add_argument('--cache-dir', type=Path,
                        default=Path(os.environ.get('CA_SHELTER_RPM_CACHE', str(root / 'target/dependencies/shelter'))))
    args = parser.parse_args()
    try:
        print(resolve(args.manifest.resolve(), args.cache_dir.resolve()))
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(1, 'Cannot resolve companion Shelter RPM: ' + str(error) + '\n')


if __name__ == '__main__':
    main()
