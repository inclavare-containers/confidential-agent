#!/usr/bin/env python3.11
"""Package a relocatable deployment ZIP from native binaries and a provider mirror."""

import argparse
from pathlib import Path
import re
import shutil
import tempfile
import zipfile


ROOT = Path(__file__).resolve().parent.parent
NATIVE_TOOLS = (
    'aliyun', 'ossutil64', 'oras', 'tng', 'attestation-challenge-client',
    'terraform', 'jq', 'cosign',
)


def copy_executable(source, target):
    shutil.copyfile(source, target)
    target.chmod(0o755)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=('linux-amd64', 'darwin-arm64'), required=True)
    parser.add_argument('--cagent', type=Path, required=True)
    parser.add_argument('--shelter', type=Path, required=True)
    parser.add_argument('--native-dir', type=Path, required=True)
    parser.add_argument('--providers', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()

    version = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]
    name = f'cagent-deploy-{version}-{args.platform}'
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output) as directory:
        bundle = Path(directory) / name
        native = bundle / 'bin'
        native.mkdir(parents=True)
        copy_executable(args.cagent, native / 'cagent')
        copy_executable(args.shelter, native / 'shelter')
        for tool in NATIVE_TOOLS:
            copy_executable(args.native_dir / tool, native / tool)
        shutil.copytree(args.providers, bundle / 'provider')
        copy_executable(ROOT / 'packaging/deploy-one-click.sh', bundle / 'one-click.sh')

        share = bundle / 'share/cagent'
        share.mkdir(parents=True)
        shutil.copy2(ROOT / 'software-versions.json', share / 'software-versions.json')
        for path in (
            'one-click/lib', 'common', 'examples/allinone', 'examples/openclaw',
            'examples/cli-agents', 'examples/hermes-agent',
        ):
            shutil.copytree(ROOT / path, share / path)
        (share / 'hack').mkdir()
        for path in ('fetch-release-image.py', 'download-release-image.py', 'release-image.json'):
            shutil.copy2(ROOT / 'hack' / path, share / 'hack' / path)

        archive = Path(directory) / (name + '.zip')
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=3) as zipped:
            for path in sorted(bundle.rglob('*')):
                if path.is_file():
                    zipped.write(path, path.relative_to(bundle.parent))
        archive.replace(output / archive.name)
    print(output / archive.name)


if __name__ == '__main__':
    main()
