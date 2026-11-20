#!/usr/bin/env bash
set -euo pipefail

for candidate in python3.11 python3; do
  if command -v "$candidate" >/dev/null && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then
    exec "$candidate" "$@"
  fi
done

version=3.11.18
release=20261110
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)
    target=x86_64-unknown-linux-gnu
    sha256=3288c54cea3db350052019b3f8b558dd8a3da8afa6fda573f5fd2bcb37635b4a
    checksum=(sha256sum)
    ;;
  Darwin/arm64)
    target=aarch64-apple-darwin
    sha256=3afb0567b2a2b30b3a7341bd143cb4e8a69f3d8e95d4641218eaa45f0fabbd07
    checksum=(shasum -a 256)
    ;;
  *) echo '[one-click] Install Python 3.11 or newer on this platform.' >&2; exit 1 ;;
esac
parent="${XDG_DATA_HOME:-$HOME/.local/share}/cagent/one-click/python/$version+$release-$target"
interpreter="$parent/python/bin/python3"
if [[ -x "$interpreter" ]]; then exec "$interpreter" "$@"; fi
if [[ "${CA_SKIP_DEPS:-0}" == 1 ]]; then
  echo '[one-click] Python 3.11 or newer is required for this step. Rerun without --skip-deps to install it for this user.' >&2
  exit 1
fi

asset="cpython-$version+$release-$target-install_only_stripped.tar.gz"
url="https://github.com/astral-sh/python-build-standalone/releases/download/$release/$asset"
urls=("$url")
proxy="${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}"
if [[ -n "$proxy" ]]; then
  case "${CA_GITHUB_PROXY_FIRST:-1}" in
    1|true|TRUE|yes|YES|proxy|proxy-first) urls=("${proxy%/}/$url" "$url") ;;
    *) urls+=("${proxy%/}/$url") ;;
  esac
fi

echo "[one-click] Installing Python $version in $parent/python" >&2
mkdir -p "$parent"
(
  stage="$(mktemp -d "$parent/.install.XXXXXX")"
  trap 'rm -rf "$stage"' EXIT
  for candidate in "${urls[@]}"; do
    if curl -fsSL --retry 2 --connect-timeout 15 --max-time 300 -o "$stage/archive" "$candidate" &&
       (cd "$stage" && printf '%s  archive\n' "$sha256" | "${checksum[@]}" -c - >&2); then
      tar -xzf "$stage/archive" -C "$stage"
      # Concurrent runs can finish extracting the same release together.
      mv "$stage/python" "$parent/" || [[ -x "$interpreter" ]]
      exit 0
    fi
  done
  echo "[one-click] Failed to download a verified copy of $asset" >&2
  exit 1
)
exec "$interpreter" "$@"
