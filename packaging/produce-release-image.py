#!/usr/bin/env python3
"""Build the release guest once, then push its complete Shelter artifact."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SOURCE = json.loads((ROOT / 'hack/release-image.json').read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    steps = parser.add_subparsers(dest='step', required=True)
    build = steps.add_parser('build', help='build from the checked-out release commit')
    build.add_argument('work_dir', type=Path)
    push = steps.add_parser('push', help='export metadata and push the artifact from that build')
    push.add_argument('work_dir', type=Path)
    push.add_argument('output_dir', type=Path)
    push.add_argument('--repository', default=SOURCE['oci_repository'], help='push directly here or to the promotion staging repository')
    args = parser.parse_args()
    work = args.work_dir.resolve()
    ca = [os.environ.get('CA_BIN', 'confidential-agent'), '--state-dir', work / 'state']
    service = 'allinone-builder'
    builder = work / service
    provenance = work / 'build-source.json'

    if args.step == 'build':
        if subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT):
            parser.exit(1, 'Build from a clean release checkout\n')
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        subprocess.run([*ca, 'init', '--stage', 'build', '--non-interactive',
                        '--reference-values', 'rekor', '--output-dir', work], cwd=ROOT, check=True)
        subprocess.run([*ca, 'build', '--spec', builder / 'build.yaml'], cwd=builder, check=True)
        provenance.write_text(json.dumps({'version': SOURCE['version'], 'source_commit': commit}) + '\n')
        return

    index = json.loads(provenance.read_text())
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    release = output / 'image-release.json'
    subprocess.run([*ca, 'image', 'export', service, '--output', release], check=True)
    index['release_digest'] = 'sha256:' + hashlib.sha256(release.read_bytes()).hexdigest()
    shelter = os.environ.get('CA_SHELTER_BIN', 'shelter')
    manifest = json.loads((work / 'state/services' / service / 'manifest.json').read_text())
    command = [shelter, '--work-dir', manifest['shelter_work_dir'], 'image']
    inspection = json.loads(subprocess.check_output([*command, 'inspect-release', release, '--metadata-only',
                                                     '--release-digest', index['release_digest']]))
    if not inspection['rekor_value']:
        parser.exit(1, 'Release build is missing its Rekor evidence\n')
    tag = args.repository + ':v' + index['version']
    oras = Path(os.environ.get('CA_TOOLS_DIR', '/usr/libexec/cagent/bin')) / 'oras'
    result = json.loads(subprocess.check_output([*command, 'push', manifest['shelter_build_id'], tag,
                                                 '--oras', oras, '--format', 'json']))
    index['oci_reference'] = SOURCE['oci_repository'] + '@' + result['artifact']['digest']
    (output / 'image-index.json').write_text(json.dumps(index, indent=2) + '\n')
    print(json.dumps(index))


if __name__ == '__main__':
    main()
