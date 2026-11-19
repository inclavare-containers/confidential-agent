#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destination="${1:?usage: packaging/fetch-native-tools.sh OUTPUT_DIR [CACHE_DIR] [oras]}"
cache="${2:-$root/target/native-tool-downloads}"
pins="$root/packaging/native-tools.tsv"
if [[ "${3:-}" == oras ]]; then
  pins="$root/packaging/companion-tools.tsv"
fi
[[ "$(uname -m)" == x86_64 ]] || { echo 'Native tool pins currently support x86_64' >&2; exit 1; }
mkdir -p "$destination" "$cache"
work="$(mktemp -d "$cache/.fetch.XXXXXX")"
trap 'rm -rf "$work"' EXIT

while read -r name version sha256 url; do
  [[ -n "$name" && "$name" != \#* ]] || continue
  archive="$cache/$name-$version"
  if ! printf '%s  %s\n' "$sha256" "$archive" | sha256sum -c - >/dev/null 2>&1; then
    urls=("$url")
    if [[ "$url" == https://github.com/* && -n "${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}" ]]; then
      proxy="${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}"
      if [[ "${CA_GITHUB_PROXY_FIRST:-1}" == 1 ]]; then
        urls=("${proxy%/}/$url" "$url")
      else
        urls+=("${proxy%/}/$url")
      fi
    fi
    downloaded=0
    for candidate in "${urls[@]}"; do
      if curl -fSL --retry 2 --connect-timeout 20 --max-time 300 \
          --speed-limit 65536 --speed-time 20 -o "$work/download" "$candidate" &&
         printf '%s  %s\n' "$sha256" "$work/download" | sha256sum -c -; then
        mv "$work/download" "$archive"
        downloaded=1
        break
      fi
    done
    [[ "$downloaded" == 1 ]] || { echo "Failed to fetch verified $name $version" >&2; exit 1; }
  fi
  if [[ "$name" == aliyun || "$name" == oras ]]; then
    tar -xzf "$archive" -C "$work" "$name"
    install -m 0755 "$work/$name" "$destination/$name"
  else
    install -m 0755 "$archive" "$destination/$name"
  fi
done < "$pins"
install -m 0644 "$pins" "$destination/SOURCES.tsv"
