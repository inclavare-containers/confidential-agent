#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/common/software-versions.sh"
export CA_SOFTWARE_VERSIONS_FILE="$ROOT_DIR/software-versions.json"
source <(sed -n '/^install_dingtalk_extension() {/,/^}/p' "$ROOT_DIR/examples/openclaw/files/install-openclaw-runtime.sh")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
OPENCLAW_USER=root
OPENCLAW_USER_HOME="$work"
OPENCLAW_CONFIG_DIR="$work/.openclaw"
OPENCLAW_BIN="$work/openclaw"
cat > "$OPENCLAW_BIN" <<'OPENCLAW'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$2" == inspect ]]; then
    jq -n --arg integrity "$DINGTALK_RECEIPT_INTEGRITY" '{install: {integrity: $integrity}}'
fi
OPENCLAW
chmod +x "$OPENCLAW_BIN"

export DINGTALK_RECEIPT_INTEGRITY='sha256-unexpected'
if install_dingtalk_extension >"$work/result" 2>&1; then
    echo 'An unexpected DingTalk archive was accepted' >&2
    exit 1
fi
grep -Fq 'DingTalk package integrity does not match the checked-in pin' "$work/result"

export DINGTALK_RECEIPT_INTEGRITY="$(ca_software_version dingtalk_integrity)"
install_dingtalk_extension
printf '%s\n' 'DingTalk installation receipt matches the pinned archive: PASS'
