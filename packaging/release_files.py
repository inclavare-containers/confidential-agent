"""Write the release manifest and checksums for the assembled release directory."""
import hashlib
import json


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def write_manifest(directory, manifest):
    path = directory / 'release-manifest.json'
    sums = directory / 'SHA256SUMS'
    files = {p.name: digest(p) for p in sorted(directory.iterdir()) if p not in (path, sums)}
    manifest['files'] = files
    path.write_text(json.dumps(manifest, indent=2) + '\n')
    checksums = files.copy()
    checksums[path.name] = digest(path)
    sums.write_text(''.join(checksums[name] + '  ' + name + '\n' for name in sorted(checksums)))
