#!/bin/sh
# Load the selected source's question phase before the installer changes the host.
set -eu

first="$1"
second="$2"
entrypoint="$3"
arguments="$4"
stage="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

fetch_frontend_file() {
    url="$first/$1"
    if ! curl -fsSL --retry 2 --connect-timeout 10 --max-time 60 -o "$stage/$1" "$url"; then
        [ -n "$second" ] || return 1
        url="$second/$1"
        curl -fsSL --retry 2 --connect-timeout 10 --max-time 60 -o "$stage/$1" "$url"
    fi
}

for path in common/software-versions.sh software-versions.json \
    one-click/lib/common.sh one-click/lib/deps-alinux.sh one-click/lib/credentials.sh \
    one-click/lib/openclaw.sh one-click/lib/harness.sh one-click/lib/deploy.sh \
    one-click/lib/cpu-state.sh one-click/lib/main.sh; do
    mkdir -p "$stage/$(dirname "$path")"
    fetch_frontend_file "$path"
done

fetch_frontend_file "one-click/$entrypoint"
CA_BOOTSTRAP_HOOK="$stage/one-click/$entrypoint"
CA_ONE_CLICK_ENTRY_URL="$url"
export CA_BOOTSTRAP_HOOK CA_ONE_CLICK_ENTRY_URL
chmod u+x "$CA_BOOTSTRAP_HOOK"
if [ -s "$arguments" ]; then
    exec xargs -0 -a "$arguments" bash "$stage/one-click/lib/main.sh"
fi
exec bash "$stage/one-click/lib/main.sh"
