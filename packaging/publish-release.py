#!/usr/bin/env python3
"""Upload a tested binary release, preserving already uploaded asset contents."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from release_files import digest, write_manifest


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--reuse-image-from', help='published release tag providing the same tested guest image')
    args = parser.parse_args()
    directory = args.directory.resolve()
    manifest = json.loads((directory / 'release-manifest.json').read_text())
    tag = manifest['tag']
    repo = os.environ['GITHUB_REPOSITORY']
    assert repo == 'inclavare-containers/confidential-agent'
    assert re.fullmatch(r'v\d+\.\d+\.\d+', tag)
    assert manifest.get('source_dirty') is False, 'Release requires a clean source checkout'
    assert os.environ['GITHUB_REF'] == 'refs/tags/' + tag
    commit = gh('api', 'repos/' + repo + '/commits/' + tag, '--jq', '.sha').strip()
    assert commit == manifest['source_commit'], 'Tag and tested artifact source differ'
    subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=directory, check=True)
    # GitHub's /releases/tags endpoint omits unpublished drafts. The CLI also
    # resolves drafts, then the numeric API URL gives complete asset metadata.
    release_url = gh('release', 'view', tag, '--repo', repo, '--json', 'apiUrl', '--jq', '.apiUrl').strip()
    assert release_url.startswith('https://api.github.com/repos/' + repo + '/releases/')
    release = json.loads(gh('api', release_url))
    assert release['draft'], 'Published releases are immutable in this workflow'
    existing = {a['name']: a for a in release['assets']}
    with tempfile.TemporaryDirectory() as tmp:
        image_tag = args.reuse_image_from or tag
        if args.reuse_image_from:
            assert re.fullmatch(r'v\d+\.\d+\.\d+', image_tag), 'Expected a versioned release tag'
            published = gh('release', 'view', image_tag, '--repo', repo, '--json', 'isDraft', '--jq', '.isDraft')
            assert published.strip() == 'false', 'Reuse requires a published guest image'
        gh('release', 'download', image_tag, '--repo', repo, '--pattern', 'image-index.json',
           '--pattern', 'image-release.json', '--dir', tmp)
        resolver = Path(__file__).resolve().parents[1] / 'hack/fetch-release-image.py'
        index = json.loads(subprocess.check_output([
            sys.executable, resolver, '--from-dir', tmp, '--output-dir', directory, '--version', image_tag[1:]]))
        if args.reuse_image_from:
            path = directory / 'image-index.json'
            published_index = json.loads(path.read_text())
            published_index['version'] = manifest['version']
            path.write_text(json.dumps(published_index, indent=2) + '\n')
        else:
            assert index['source_commit'] == commit, 'Image and RPMs were built from different commits'
        config = Path(tmp) / 'anonymous.json'
        config.write_text('{"auths":{}}')
        oci = json.loads(subprocess.check_output([
            'oras', 'manifest', 'fetch', '--registry-config', str(config), index['oci_reference']]))
        raw = json.loads((directory / 'image-release.json').read_text())
        assert oci['artifactType'] == 'application/vnd.shelter.artifact.v1'
        assert oci['annotations']['io.shelter.manifest.digest'] == raw['expected']['manifest_digest'], \
            'Official OCI artifact differs from image-release.json'
    write_manifest(directory, manifest)
    for path in sorted(directory.iterdir()):
        assert path.is_file() and path.stat().st_size < 2 * 1024**3
        if path.name in existing:
            asset = existing[path.name]
            actual = asset.get('digest')
            if actual:
                assert actual == 'sha256:' + digest(path), 'Existing asset differs: ' + path.name
            else:
                with tempfile.TemporaryDirectory() as tmp:
                    downloaded = Path(tmp) / path.name
                    gh('release', 'download', tag, '--repo', repo, '--pattern', path.name, '--output', str(downloaded))
                    assert digest(downloaded) == digest(path), 'Existing asset differs: ' + path.name
            continue
        gh('release', 'upload', tag, str(path), '--repo', repo)
    # Read back asset names, byte sizes and GitHub's computed hashes before publishing.
    release = json.loads(gh('api', release_url))
    assets = {a['name']: a for a in release['assets']}
    for path in directory.iterdir():
        assert path.name in assets and assets[path.name]['size'] == path.stat().st_size
        if assets[path.name].get('digest'):
            assert assets[path.name]['digest'] == 'sha256:' + digest(path)
    notes = Path(__file__).resolve().parent.parent / 'docs/releases' / (tag + '.md')
    gh('release', 'edit', tag, '--repo', repo, '--target', commit, '--title', tag,
       '--notes-file', str(notes), '--draft=false', '--latest')
    print('Published ' + repo + ' ' + tag)


if __name__ == '__main__':
    main()
