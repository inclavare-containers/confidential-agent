#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/openclaw.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
STATE_DIR="$WORK_DIR/state"
E2E_RUN_ID=probe-failure-test
E2E_RUN_TDX_SKILL_PROBE=1
touch "$WORK_DIR/key"
state_value() {
  case "$3" in
    deploy.public_ip) printf '127.0.0.1\n' ;;
    build.debug_ssh.private_key) printf '%s/key\n' "$WORK_DIR" ;;
    *) return 1 ;;
  esac
}
wait_for_ssh() { :; }
record_file_as_block() { :; }
ssh_guest() {
  shift 2
  local command="$*"
  case "$command" in
    "umask 077; cat > "*) cat >/dev/null ;;
    "node "*) printf 'simulated probe failure\n' >&2; return 1 ;;
    "cat "*)
      if [[ "$command" == *".gateway.log"* ]]; then
        printf 'gateway\n' >>"$WORK_DIR/events"
        printf 'synthetic gateway diagnostics\n'
      elif [[ "$command" == *".pep.log"* ]]; then
        printf 'pep\n' >>"$WORK_DIR/events"
        printf 'synthetic PEP diagnostics\n'
      else
        printf 'report\n' >>"$WORK_DIR/events"
        printf '%s\n' '{"ok":false,"checks":["allow"],"isolationEvidence":{"kernel":{"CapEff":"0000000000000001","CapBnd":"0000000000000000"}},"error":"retained effective capability"}'
      fi ;;
    "rm -f "*) printf 'delete\n' >>"$WORK_DIR/events" ;;
    *) printf 'unexpected remote operation\n' >&2; return 1 ;;
  esac
}
if run_openclaw_pep_e2e 1234 synthetic-token; then
  echo 'failed probe unexpectedly succeeded' >&2
  exit 1
fi
jq -e '.ok == false and .checks == ["allow"] and
  .isolationEvidence.kernel.CapEff == "0000000000000001"' \
  "$WORK_DIR/pep-enforcement-probe.json" >/dev/null
cmp "$WORK_DIR/pep-enforcement-probe.remote.json" "$WORK_DIR/pep-enforcement-probe.json"
[[ "$(cat "$WORK_DIR/events")" == $'report\ngateway\npep\ndelete' ]]
echo 'PEP failure report and isolation evidence fetched before remote cleanup: PASS'
