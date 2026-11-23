#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
CASE_NAME=connect-cleanup-test
source "$ROOT_DIR/tools/e2e/lib/common.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
CA_BIN="$WORK_DIR/fake-ca"
cat > "$CA_BIN" <<'SH'
#!/usr/bin/env bash
while (($#)); do
  if [[ "$1" == --ready-json ]]; then
    printf '{"client_endpoints":[{"local_port":12345}]}\n' > "$2"
    exit 0
  fi
  shift
done
exit 1
SH
chmod +x "$CA_BIN"
record() { :; }
record_file_as_block() { :; }
curl() { return 0; }
cleanup_connect_ready() {
  [[ -f "$1" ]] || return 0
  printf '%s\n' "$1" >> "$WORK_DIR/cleaned"
}
port="$(start_connect_until_http_ready "$WORK_DIR/state" probe /ready 1 5)"
[[ "$port" == 12345 ]]
[[ "${#E2E_CONNECT_READY_FILES[@]}" == 0 ]]
cleanup_connects
grep -Fxq "$WORK_DIR/probe-connect-attempt-1-ready.json" "$WORK_DIR/cleaned"
printf 'E2E subshell connect cleanup test: PASS\n'
