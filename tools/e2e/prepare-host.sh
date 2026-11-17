#!/usr/bin/env bash
# Install the pinned host clients required by the cloud E2E flows.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT_DIR/one-click/lib/common.sh"
source "$ROOT_DIR/common/software-versions.sh"
source "$ROOT_DIR/one-click/lib/deps-alinux.sh"
CA_NODE_VERSION="$(ca_software_version node)"
CA_OPENCLAW_VERSION="$(ca_software_version openclaw)"
CA_SKIP_HOST_OPENCLAW=0
CA_NPM_REGISTRY="${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
ensure_host_openclaw_runtime
node --version
openclaw --version
