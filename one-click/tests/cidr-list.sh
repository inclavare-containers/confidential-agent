#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=one-click/lib/common.sh
. "$ROOT_DIR/one-click/lib/common.sh"
# shellcheck source=one-click/lib/credentials.sh
. "$ROOT_DIR/one-click/lib/credentials.sh"
# shellcheck source=one-click/lib/deploy.sh
. "$ROOT_DIR/one-click/lib/deploy.sh"

fail() {
  printf 'not ok - %s\n' "$*" >&2
  exit 1
}

assert_eq() {
  local expected="$1"
  local actual="$2"
  local message="$3"
  if [[ "$actual" != "$expected" ]]; then
    printf 'not ok - %s\nexpected:\n%s\nactual:\n%s\n' "$message" "$expected" "$actual" >&2
    exit 1
  fi
}

test_normalize_cidr_list() {
  local normalized invalid
  normalize_cidr_list "203.0.113.10/32,198.51.100.0/24 203.0.113.10/32;192.0.2.0/24" normalized invalid \
    || fail "normalize_cidr_list rejected a valid mixed separator list"
  assert_eq "203.0.113.10/32 198.51.100.0/24 192.0.2.0/24" "$normalized" \
    "normalize_cidr_list should split, validate, and deduplicate CIDRs"

  if normalize_cidr_list "203.0.113.999/32" normalized invalid; then
    fail "normalize_cidr_list accepted an invalid CIDR"
  fi
  assert_eq "203.0.113.999/32" "$invalid" "normalize_cidr_list should report the invalid token"
}

write_fake_ca() {
  local path="$1"
  cat >"$path" <<'FAKE'
#!/usr/bin/env bash
set -Eeuo pipefail

args=()
while (($# > 0)); do
  case "$1" in
    --tools-dir|--state-dir)
      shift 2
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done
set -- "${args[@]}"

case "${1:-} ${2:-}" in
  "peering show")
    exit 1
    ;;
  "peering add")
    cidr=""
    label=""
    shift 2
    while (($# > 0)); do
      case "$1" in
        --cidr)
          cidr="$2"
          shift 2
          ;;
        --label)
          label="$2"
          shift 2
          ;;
        *)
          shift
          ;;
      esac
    done
    printf 'add %s %s\n' "$label" "$cidr" >>"$FAKE_CA_LOG"
    ;;
  "peering remove")
    printf 'remove %s\n' "${3:-}" >>"$FAKE_CA_LOG"
    ;;
  *)
    printf 'unexpected command: %s\n' "$*" >&2
    exit 2
    ;;
esac
FAKE
  chmod +x "$path"
}

test_ensure_operator_peering_labels_and_deployer() {
  local tmp_dir actual
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' RETURN
  write_fake_ca "$tmp_dir/confidential-agent"

  export FAKE_CA_LOG="$tmp_dir/commands.log"
  CA_BIN="$tmp_dir/confidential-agent"
  CA_TOOLS_DIR="test-tools"
  CA_STATE_DIR="$tmp_dir/state"
  CA_WORK_DIR="$tmp_dir/work"
  CA_NON_INTERACTIVE=1
  CA_ASSUME_YES=1

  CA_ALLOWED_CIDRS="203.0.113.10/32 198.51.100.0/24"
  CA_ALLOWED_CIDR="$CA_ALLOWED_CIDRS"
  CA_DEPLOYER_CIDR="198.51.100.1/32"
  ensure_operator_peering
  actual="$(cat "$FAKE_CA_LOG")"
  assert_eq $'add ops 203.0.113.10/32\nadd ops-2 198.51.100.0/24\nadd deployer 198.51.100.1/32' "$actual" \
    "ensure_operator_peering should label multiple CIDRs and add a distinct deployer CIDR"

  : >"$FAKE_CA_LOG"
  CA_ALLOWED_CIDRS="203.0.113.10/32"
  CA_ALLOWED_CIDR="$CA_ALLOWED_CIDRS"
  CA_DEPLOYER_CIDR="203.0.113.10/32"
  ensure_operator_peering
  actual="$(cat "$FAKE_CA_LOG")"
  assert_eq "add ops 203.0.113.10/32" "$actual" \
    "ensure_operator_peering should not add deployer when it is already allowed"

  : >"$FAKE_CA_LOG"
  CA_ALLOWED_CIDRS="0.0.0.0/0"
  CA_ALLOWED_CIDR="$CA_ALLOWED_CIDRS"
  CA_DEPLOYER_CIDR="203.0.113.10/32"
  ensure_operator_peering
  actual="$(cat "$FAKE_CA_LOG")"
  assert_eq "add ops 0.0.0.0/0" "$actual" \
    "ensure_operator_peering should not add deployer when all IPv4 sources are allowed"
}

test_normalize_cidr_list
test_ensure_operator_peering_labels_and_deployer
printf 'ok - one-click CIDR list tests passed\n'
