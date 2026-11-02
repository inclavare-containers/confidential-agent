#!/usr/bin/env bash
# Install only the checksum-verified RPM pair recorded by a binary release.
set -euo pipefail
version=0.2.0
source_dir=""
download_only=0
destination=""
while (($#)); do
  case "$1" in
    --version) version="${2:?missing version}"; shift 2 ;;
    --from-dir) source_dir="${2:?missing directory}"; shift 2 ;;
    --download-only) download_only=1; shift ;;
    --output-dir) destination="${2:?missing directory}"; shift 2 ;;
    --help|-h)
      printf '%s\n' 'Usage: bash install-rpm.sh [--version 0.2.0] [--from-dir DIR] [--download-only --output-dir DIR]'
      exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Invalid release version' >&2; exit 2; }
[[ "$(uname -m)" == x86_64 ]] || { echo 'This release supports x86_64 only' >&2; exit 1; }
if [[ "$download_only" == 0 ]]; then
  [[ "$EUID" == 0 ]] || { echo 'Run as root to install RPMs' >&2; exit 1; }
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${VERSION_ID:-}" == 3 && ( "${ID:-}" == alinux || "${VARIANT_ID:-}" == openanolis || -n "${ALINUX_MINOR_ID:-}" ) ]] || {
    echo 'This release supports Alibaba Cloud Linux 3 only' >&2; exit 1;
  }
fi
for tool in sha256sum rpm; do
  command -v "$tool" >/dev/null || { echo "Required tool not found: $tool" >&2; exit 1; }
done
# Minimal Alinux3 images provide DNF's Python without a python3 command.
python="$(command -v python3 || true)"
if [[ -z "$python" && -x /usr/libexec/platform-python ]]; then
  python=/usr/libexec/platform-python
fi
[[ -n "$python" ]] || { echo 'Python 3 is required to verify the release manifest' >&2; exit 1; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
base="https://github.com/inclavare-containers/confidential-agent/releases/download/v$version"
validate_manifest() {
"$python" - "$1" "$version" <<'PY'
import json, re, sys
d = json.load(open(sys.argv[1]))
assert d['schema'] == 'confidential-agent/binary-release/v1'
assert d['version'] == sys.argv[2] and d['tag'] == 'v' + sys.argv[2]
assert d['platform'] == 'alinux3-x86_64'
packages = d['packages']
assert len(packages) == 2 and {p['name'] for p in packages} == {'cagent', 'shelter'}
for p in packages:
    assert p['arch'] == 'x86_64'
    assert re.fullmatch(r'[A-Za-z0-9._+-]+\.rpm', p['file'])
    assert re.fullmatch(r'[0-9a-f]{64}', p['sha256'])
    assert re.fullmatch(r'[0-9a-f]{64}', p['payload_sha256'])
    assert p['file'] == '{name}-{version}-{release}.{arch}.rpm'.format(**p)
    assert p['name'] != 'cagent' or p['version'] == sys.argv[2]
    print(p['file'], p['sha256'], p['payload_sha256'], sep='\t')
PY
}

download() {
  local name="$1" dest="$2" expected="${3:-}" url proxy
  if [[ -n "$source_dir" ]]; then
    cp "$source_dir/$name" "$dest"
    [[ -z "$expected" ]] || printf '%s  %s\n' "$expected" "$dest" | sha256sum -c - >&2
    return
  fi
  local urls=("$base/$name")
  proxy="${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}"
  if [[ -n "$proxy" ]]; then
    if [[ "${CA_GITHUB_PROXY_FIRST:-1}" == 1 ]]; then urls=("${proxy%/}/$base/$name" "$base/$name")
    else urls+=("${proxy%/}/$base/$name"); fi
  fi
  for url in "${urls[@]}"; do
    if curl -fSL --retry 2 --connect-timeout 20 --max-time 600 -o "$dest" "$url"; then
      if [[ -n "$expected" ]]; then
        if printf '%s  %s\n' "$expected" "$dest" | sha256sum -c - >&2; then return; fi
      elif validate_manifest "$dest" >/dev/null; then
        return
      fi
    fi
  done
  echo "Failed to download verified $name" >&2
  return 1
}
download release-manifest.json "$work/release-manifest.json"
validate_manifest "$work/release-manifest.json" > "$work/packages.tsv"
packages=()
while IFS=$'\t' read -r name digest payload; do
  download "$name" "$work/$name" "$digest"
  [[ "$(rpm -qp --qf '%{PAYLOADDIGEST}' "$work/$name")" == "$payload" ]] || {
    echo "RPM payload mismatch: $name" >&2; exit 1;
  }
  packages+=("$work/$name")
done < "$work/packages.tsv"
if [[ "$download_only" == 1 ]]; then
  [[ -n "$destination" ]] || { echo '--download-only requires --output-dir' >&2; exit 2; }
  mkdir -p "$destination"
  cp "$work/release-manifest.json" "${packages[@]}" "$destination/"
  echo "Verified RPMs saved to $destination"
else
  dnf install -y "${packages[@]}"
  confidential-agent --version
  rpm -q cagent shelter attestation-challenge-client libtdx-verify trusted-network-gateway
fi
