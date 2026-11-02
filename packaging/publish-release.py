#!/usr/bin/env python3
"""Upload a tested binary release, preserving already uploaded asset contents."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True)


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def main():
    directory = Path(sys.argv[1]).resolve()
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
