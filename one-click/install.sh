#!/bin/sh
# Install the verified binary release. Deployment is owned by `cagent up`.
set -eu
version=0.2.0
previous=
for argument in "$@"; do
    if [ "$previous" = --version ]; then version=$argument; fi
    previous=$argument
done
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." 2>/dev/null && pwd)
if [ -f "$root/packaging/install-rpm.sh" ]; then
    exec bash "$root/packaging/install-rpm.sh" "$@"
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
curl -fSL --retry 2 --connect-timeout 20 \
    "https://github.com/inclavare-containers/confidential-agent/releases/download/v$version/install-rpm.sh" \
    -o "$work/install-rpm.sh"
bash "$work/install-rpm.sh" "$@"
printf '%s\n' 'Installed. Run cagent up to initialize and deploy, or cagent cleanup --all to destroy recorded instances.'
