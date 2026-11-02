#!/usr/bin/env python3.11
"""Collect an exact, versioned pair of RPMs and offline cagent build inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('rpm_root', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    version = re.search(r'^version = "([^"]+)"', (root / 'Cargo.toml').read_text(), re.M)[1]
    spec = (root / 'cagent.spec').read_text()
    assert re.search(r'^Version:\s+(\S+)', spec, re.M)[1] == version
    release = re.search(r'^Release:\s+(\d+)', spec, re.M)[1]
    companion = json.loads((root / 'hack/shelter-rpm.json').read_text())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise SystemExit('Release output must be empty to prevent stale artifacts')
    files = [args.rpm_root / 'RPMS/x86_64' / ('cagent-' + version + '-' + release + '.al8.x86_64.rpm'),
             args.rpm_root / 'RPMS/x86_64' / companion['file']]
    packages = []
    for path in files:
        name, actual_version, actual_release, arch, payload = subprocess.check_output(
            ['rpm', '-qp', '--qf', '%{NAME}\n%{VERSION}\n%{RELEASE}\n%{ARCH}\n%{PAYLOADDIGEST}', str(path)],
            text=True).splitlines()
        digest = sha256(path)
        if name == 'cagent':
            assert actual_version == version and actual_release == release + '.al8' and arch == 'x86_64'
        else:
            assert name == 'shelter' and digest == companion['sha256'] and payload == companion['payload_sha256']
        shutil.copy2(path, output / path.name)
        packages.append({'name': name, 'version': actual_version, 'release': actual_release,
                         'arch': arch, 'file': path.name, 'sha256': digest, 'payload_sha256': payload})
    srpm = args.rpm_root / 'SRPMS' / ('cagent-' + version + '-' + release + '.al8.src.rpm')
    shutil.copy2(srpm, output / srpm.name)
    inputs = ['cagent-' + version + '.tar.gz', 'cagent-vendor.tar.gz',
              'cagent-native-tools.tar.gz', 'cagent.spec', 'SHA256SUMS', 'build-source.json']
    with tarfile.open(output / ('cagent-' + version + '-build-inputs.tar.gz'), 'w:gz') as archive:
        for name in inputs:
            archive.add(args.rpm_root / 'SOURCES' / name, arcname='SOURCES/' + name)
    for source in ['hack/shelter-rpm.json', 'software-versions.json', 'packaging/install-rpm.sh',
                   'packaging/native-tools.tsv', 'packaging/release-build.json']:
        path = root / source
        shutil.copy2(path, output / path.name)
    environment = args.rpm_root / 'build-environment.txt'
    if environment.exists():
        shutil.copy2(environment, output / environment.name)
    source_info = json.loads((args.rpm_root / 'SOURCES/build-source.json').read_text())
    assert source_info['version'] == version
    commit, dirty = source_info['source_commit'], source_info['source_dirty']
    manifest = {'schema': 'confidential-agent/binary-release/v1', 'version': version,
                'tag': 'v' + version, 'source_commit': commit, 'source_dirty': dirty, 'platform': 'alinux3-x86_64',
                'packages': packages, 'shelter': companion,
                'files': {p.name: sha256(p) for p in sorted(output.iterdir())}}
    (output / 'release-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    (output / 'SHA256SUMS').write_text(''.join(
        sha256(p) + '  ' + p.name + '\n' for p in sorted(output.iterdir()) if p.name != 'SHA256SUMS'))
    print(json.dumps({'version': version, 'packages': packages, 'output': str(output)}))


if __name__ == '__main__':
    main()
