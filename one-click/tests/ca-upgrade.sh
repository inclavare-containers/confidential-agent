#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cat >"$work/candidate" <<'CANDIDATE'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "--state-dir $TEST_STATE upgrade check" ]]
exit "$TEST_CHECK_EXIT"
CANDIDATE
chmod +x "$work/candidate"
cat >"$work/runner" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail
source "$TEST_REPO/one-click/lib/deploy.sh"
CA_STATE_DIR="$TEST_STATE"
CA_BIN="$TEST_WORK/candidate"
CA_AGENTD_BIN="$CA_BIN"
CA_GATEWAY_BIN="$CA_BIN"
CA_DISABLE_PEP=1
is_root() { return 0; }
die() { echo "$*" >&2; exit 1; }
log() { :; }
install() { printf '%s\n' "$*" >>"$TEST_WORK/installed"; }
install_confidential_agent_cli
RUNNER
if TEST_REPO="$repo" TEST_WORK="$work" TEST_STATE="$work/state" TEST_CHECK_EXIT=1 \
    bash "$work/runner" >"$work/error" 2>&1; then
  echo 'legacy upgrade unexpectedly succeeded' >&2
  exit 1
fi
[[ ! -e "$work/installed" ]]
grep -Fq 'retain the installed CLI' "$work/error"
TEST_REPO="$repo" TEST_WORK="$work" TEST_STATE="$work/state" TEST_CHECK_EXIT=0 \
    bash "$work/runner"
[[ $(wc -l <"$work/installed") == 3 ]]
printf '%s\n' 'CA upgrade preflight before binary replacement: PASS'
