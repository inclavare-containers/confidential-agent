#!/usr/bin/env bash
# Resolve a checksum-pinned companion package, downloading it when necessary.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3.11 "$root/hack/fetch-shelter-rpm.py" "$@"
