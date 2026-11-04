#!/usr/bin/env python3.11
"""Remove installer caches and share identical immutable application files."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import uuid


def regular_files(root):
    """Do not follow package-provided directory or file symlinks."""
    if root.is_symlink() or not root.is_dir():
        return
    for directory, children, files in os.walk(root, followlinks=False):
        children[:] = sorted(name for name in children
                             if not (Path(directory) / name).is_symlink())
        for name in sorted(files):
            path = Path(directory) / name
            if stat.S_ISREG(path.lstat().st_mode):
                yield path


def exclusive_allocated_bytes(root):
    """A cache inode linked to a runtime file is not reclaimable cache space."""
    inodes = {}
    for path in regular_files(root):
        info = path.stat()
        key = (info.st_dev, info.st_ino)
        count, _, _ = inodes.get(key, (0, info.st_nlink, info.st_blocks * 512))
        inodes[key] = (count + 1, info.st_nlink, info.st_blocks * 512)
    return sum(size for count, links, size in inodes.values() if count == links)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').digest()


def remove_node_cache(cache, active_node, entrypoints):
    result = {'path': str(cache), 'removed_allocated_bytes': 0}
    if not cache.exists():
        return dict(result, status='absent')
    # n normally copies this download into /usr/local. Keep it if a custom
    # installation still runs anything from the cached tree.
    if cache.is_symlink() or cache.resolve() != cache.absolute():
        return dict(result, status='preserved-symlink')
    if any(path.resolve().is_relative_to(cache) for path in entrypoints):
        return dict(result, status='preserved-active-runtime')
    cached_node = cache / 'bin/node'
    if (not cached_node.is_file() or not active_node.is_file()
            or digest(cached_node) != digest(active_node)):
        return dict(result, status='preserved-unverified-copy')
    result['removed_allocated_bytes'] = exclusive_allocated_bytes(cache)
    shutil.rmtree(cache)
    return dict(result, status='removed')


def file_key(path, info):
    # Equal bytes alone do not make differently privileged files interchangeable.
    attributes = tuple((name, os.getxattr(path, name, follow_symlinks=False))
                       for name in sorted(os.listxattr(path, follow_symlinks=False)))
    return (info.st_dev, info.st_size, stat.S_IMODE(info.st_mode),
            info.st_uid, info.st_gid, attributes)


def deduplicate(roots, minimum_size=65536):
    candidates = {}
    for root in roots:
        for path in regular_files(root):
            info = path.stat()
            if info.st_size >= minimum_size:
                candidates.setdefault(file_key(path, info), []).append(path)
    result = {'files_relinked': 0, 'removed_allocated_bytes': 0, 'links': []}
    for paths in candidates.values():
        if len(paths) < 2:
            continue
        representatives = {}
        for path in paths:
            checksum = digest(path)
            source = representatives.setdefault(checksum, path)
            if source == path or os.path.samefile(source, path):
                continue
            info = path.stat()
            temporary = path.with_name('.ca-dedup-' + uuid.uuid4().hex)
            try:
                os.link(source, temporary, follow_symlinks=False)
                os.replace(temporary, path)
            finally:
                temporary.unlink(missing_ok=True)
            result['files_relinked'] += 1
            if info.st_nlink == 1:
                result['removed_allocated_bytes'] += info.st_blocks * 512
            result['links'].append({'path': str(path), 'source': str(source),
                                    'sha256': checksum.hex()})
    return result


def main():
    npm_cache = Path(subprocess.check_output(['npm', 'config', 'get', 'cache'], text=True).strip())
    before = exclusive_allocated_bytes(npm_cache)
    subprocess.run(['npm', 'cache', 'clean', '--force'], check=True)
    npm = {'path': str(npm_cache),
           'removed_allocated_bytes': max(0, before - exclusive_allocated_bytes(npm_cache))}

    version = subprocess.check_output(['node', '--version'], text=True).strip()
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', version):
        raise RuntimeError('unexpected Node version while locating the n download cache')
    node = Path(shutil.which('node')).resolve()
    roots = [Path(subprocess.check_output(['npm', 'root', '-g'], text=True).strip()),
             Path('/usr/local/lib/cai-hermes/repo/node_modules'),
             Path('/usr/local/lib/cai-hermes/tools'),
             Path('/usr/local/share/uv/python')]
    commands = ['node', 'npm', 'npx', 'corepack', 'openclaw', 'hermes',
                'qoder', 'opencode', 'codex', 'claude']
    entrypoints = [Path(path) for command in commands if (path := shutil.which(command))]
    cache = Path('/usr/local/n/versions/node') / version[1:]
    node_cache = remove_node_cache(cache, node, entrypoints + roots)

    # Keep all runtime and frontend build dependencies, including --dev mode.
    # Hard links preserve both package paths and work with readonly image roots.
    shared = deduplicate(roots)
    report = {'schema': 'confidential-agent/allinone-compaction/v1',
              'npm_cache': npm, 'node_download_cache': node_cache,
              'deduplication': shared}
    report['removed_allocated_bytes'] = sum(item['removed_allocated_bytes']
                                            for item in [npm, node_cache, shared])
    output = Path('/usr/local/share/confidential-agent/allinone-compaction.json')
    output.write_text(json.dumps(report, indent=2) + '\n')
    print(f"allinone: reclaimed {report['removed_allocated_bytes']} allocated bytes; "
          f"shared {shared['files_relinked']} identical files")


if __name__ == '__main__':
    main()
