#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${1:-$root/target/rpm/SOURCES}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d "$out/.prepare.XXXXXX")"
trap 'rm -rf "$work"' EXIT
cd "$root"
version="$(awk -F '"' '/^version = / {print $2; exit}' Cargo.toml)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
shelter_rpm="$("$root/hack/resolve-shelter-rpm.sh")"

# git's file list excludes local credentials, caches and build outputs. Include
# tracked working-tree changes so a package can be reviewed before its final commit.
python3.11 - "$root" "$out/cagent-$version.tar.gz" "$version" "$shelter_rpm" <<'PY'
import gzip, hashlib, json, pathlib, subprocess, sys, tarfile
root, output, version = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
manifest = json.loads((root / 'hack/shelter-rpm.json').read_text())
rpm_name = manifest['file']
if pathlib.Path(rpm_name).name != rpm_name or not rpm_name.startswith('shelter-') or not rpm_name.endswith('.rpm'):
    raise SystemExit('Invalid bundled Shelter RPM filename')
rpm_path = pathlib.Path(sys.argv[4])
with rpm_path.open('rb') as stream:
    digest = hashlib.file_digest(stream, 'sha256').hexdigest()
if digest != manifest['sha256']:
    raise SystemExit('Bundled Shelter RPM does not match hack/shelter-rpm.json; update the package and provenance together')
payload = subprocess.check_output(['rpm', '-qp', '--qf', '%{PAYLOADDIGEST}', str(rpm_path)], text=True)
if payload != manifest['payload_sha256']:
    raise SystemExit('Bundled Shelter RPM payload does not match hack/shelter-rpm.json')
paths = ['Cargo.toml', 'Cargo.lock', 'software-versions.json', 'LICENSE', 'README.md', 'cagent.spec',
         'cli', 'core', 'daemon', 'gateway', 'cai-pep', 'shelter', 'common',
         'examples', 'tools', 'packaging', 'docs', 'hack', 'one-click']
names = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--', *paths], cwd=root).split(b'\0')
epoch = int(subprocess.check_output(['git', 'show', '-s', '--format=%ct', 'HEAD'], cwd=root))
source_info = {
    'version': version,
    'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
    'source_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=root, text=True)),
}
(pathlib.Path(output).parent / 'build-source.json').write_text(json.dumps(source_info, indent=2) + '\n')
with open(output, 'wb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', mtime=epoch) as compressed, tarfile.open(fileobj=compressed, mode='w') as archive:
    for name in sorted(set(n.decode() for n in names if n)):
        path = root / name
        if not path.exists() or path.suffix == '.rpm':
            continue
        info = archive.gettarinfo(str(path), arcname=f'cagent-{version}/{name}')
        info.uid = info.gid = 0
        info.uname = info.gname = 'root'
        info.mtime = epoch
        if info.isfile():
            with path.open('rb') as stream: archive.addfile(info, stream)
        else: archive.addfile(info)
PY
cargo vendor --locked --respect-source-config "$work/vendor" >"$work/vendor-config.toml"
tar -C "$work" -czf "$out/cagent-vendor.tar.gz" vendor
"$root/packaging/fetch-native-tools.sh" "$work/native-tools"
tar -C "$work" -czf "$out/cagent-native-tools.tar.gz" native-tools
cp "$root/cagent.spec" "$out/cagent.spec"
(cd "$out" && sha256sum "cagent-$version.tar.gz" cagent-vendor.tar.gz cagent-native-tools.tar.gz > SHA256SUMS)
printf 'RPM sources: %s\n' "$out"
