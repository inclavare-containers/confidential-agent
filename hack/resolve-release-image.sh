#!/usr/bin/env bash
set -eu
exec python3.11 "$(dirname "$0")/fetch-release-image.py" "$@"
