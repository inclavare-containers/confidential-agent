#!/usr/bin/env python3
"""Fetch the image index from the official GitHub release and verify the raw Shelter metadata."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

SOURCE = json.loads(Path(__file__).with_name('release-image.json').read_text())
ASSETS = ('image-index.json', 'image-release.json')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', default=SOURCE['version'])
    parser.add_argument('--from-dir', type=Path, help='use the index and release from a trusted local directory')
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    base = f'https://github.com/inclavare-containers/confidential-agent/releases/download/v{args.version}'
    with tempfile.TemporaryDirectory(dir=output) as temp:
        stage = Path(temp)
        for name in ASSETS:
            if args.from_dir is None:
                subprocess.run(['curl', '-fsSL', '--retry', '2', '-o', stage / name, base + '/' + name], check=True)
            else:
                (stage / name).write_bytes((args.from_dir / name).read_bytes())
        index = json.loads((stage / 'image-index.json').read_text())
        if index['version'] != args.version:
            parser.exit(1, 'Image index version differs from requested release\n')
        reference = re.escape(SOURCE['oci_repository']) + r'@sha256:[0-9a-f]{64}'
        if not re.fullmatch(reference, index['oci_reference']):
            parser.exit(1, 'Expected the official OCI image pinned by digest\n')
        digest = hashlib.sha256((stage / 'image-release.json').read_bytes()).hexdigest()
        if index['release_digest'] != 'sha256:' + digest:
            parser.exit(1, 'image-release.json differs from the trusted release digest\n')
        for name in ASSETS:
            (stage / name).replace(output / name)
    print(json.dumps(dict(index, image_index=str(output / ASSETS[0]), image_release=str(output / ASSETS[1]))))


if __name__ == '__main__':
    main()
