#!/usr/bin/env bash
set -eu
bundle="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
export CA_DEPLOY_ONLY=1 CA_PACKAGED_INSTALL=1
export CA_DEPLOY_ENTRY="$bundle/one-click.sh"
export CA_BIN="$bundle/bin/cagent"
export CA_SHELTER_BIN="$bundle/bin/shelter"
export CA_TOOLS_DIR="$bundle/bin"
exec bash "$bundle/share/cagent/one-click/lib/main.sh" "$@"
