#!/usr/bin/env bash
set -euo pipefail
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/runner.sh" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$TEST_ROOT"
WORK_DIR="$TEST_ROOT/work"
mkdir -p "$WORK_DIR"
export PATH="$TEST_ROOT/bin:$PATH"
command() {
  if [[ "$*" == '-v virt-customize' ]]; then
    [[ "$TEST_HARDENING" == present || -f "$TEST_ROOT/hardening-installed" ]]
    return
  fi
  builtin command "$@"
}
if [[ "$TEST_ENTRY" == one-click ]]; then
  source "$TEST_REPO/one-click/lib/deploy.sh"
else
  source "$TEST_REPO/tools/e2e/lib/common.sh"
fi
log() { :; }
die() { echo "$*" >&2; exit 1; }
record_cmd() { :; }
record_file_as_block() { :; }
sha256sum() { printf 'fixture %s\n' "$1"; }
/usr/bin/shelter() { printf 'shelter 0.3.2\n'; }
install_shelter_rpm() {
  printf '%s\n' "$1" >>"$TEST_ROOT/installs"
  if [[ "$TEST_INSTALL" == fail ]]; then return 7; fi
  if [[ "$TEST_INSTALL" != unchanged ]]; then printf current >"$TEST_ROOT/package"; fi
  touch "$TEST_ROOT/hardening-installed"
}
install_bundled_shelter_rpm() { install_shelter_rpm "$(resolve_shelter_rpm)"; }
CA_REFERENCE_VALUES=sample
CA_MODE=deploy
CA_SLSA_GENERATOR=/unused
if [[ "$TEST_OVERRIDE" == binary ]]; then export CA_SHELTER_BIN="$TEST_ROOT/bin/shelter"; fi
if [[ "$TEST_OVERRIDE" == source ]]; then
  export E2E_USE_SOURCE_SHELTER=1
  SHELTER_DIR="$TEST_ROOT/source"
fi
if [[ "$TEST_OVERRIDE" == skip ]]; then export E2E_SKIP_SHELTER_INSTALL=1; fi
ensure_shelter
RUNNER

run_case() {
  local entry="$1" package="$2" override="$3" install="$4" expected_installs="$5" expected_success="$6"
  local hardening="${7:-present}"
  local root="$work/$entry-$package-$override-$install-$hardening"
  mkdir -p "$root/bin" "$root/hack" "$root/source/target/release" "$root/common"
  cp "$REPO_DIR/common/software-versions.sh" "$root/common/"
  printf '%s' "$package" >"$root/package"
  touch "$root/hack/shelter-0.3.2-1.al8.x86_64.rpm"
  touch "$root/hack/shelter-0.3.2-2.al8.x86_64.rpm"
  touch "$root/hack/shelter-0.3.2-3.al8.x86_64.rpm"
  printf '%s\n' '{"file":"shelter-0.3.2-2.al8.x86_64.rpm","sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","payload_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' > "$root/hack/shelter-rpm.json"
  cp "$REPO_DIR/hack/resolve-shelter-rpm.sh" "$REPO_DIR/hack/fetch-shelter-rpm.py" "$root/hack/"
  printf '#!/bin/sh\nprintf "%%s" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"\n' > "$root/bin/rpm"
  chmod +x "$root/bin/rpm"
  cat >"$root/bin/shelter" <<'BIN'
#!/usr/bin/env bash
printf 'shelter 0.3.2\n'
BIN
  cp "$root/bin/shelter" "$root/source/target/release/shelter"
  cat >"$root/hack/check-shelter.sh" <<'CHECK'
#!/usr/bin/env bash
# The old package already passes the capability check.
exit 0
CHECK
  cat >"$root/hack/check-shelter-rpm.sh" <<'CHECK'
#!/usr/bin/env bash
set -euo pipefail
[[ "$2" == "$TEST_ROOT/hack/shelter-0.3.2-2.al8.x86_64.rpm" ]]
printf '%s\n' "$1" >>"$TEST_ROOT/package-checks"
[[ "$(cat "$TEST_ROOT/package")" != old ]]
CHECK
  chmod +x "$root/bin/shelter" "$root/hack/"*.sh "$root/source/target/release/shelter"
  local status=0
  env -u CA_SHELTER_BIN -u E2E_SKIP_SHELTER_INSTALL -u E2E_USE_SOURCE_SHELTER \
    TEST_REPO="$REPO_DIR" TEST_ROOT="$root" TEST_ENTRY="$entry" \
    TEST_HARDENING="$hardening" \
    TEST_OVERRIDE="$override" TEST_INSTALL="$install" \
    bash "$work/runner.sh" >"$root/output" 2>&1 || status=$?
  if [[ "$expected_success" == yes ]]; then
    [[ "$status" == 0 ]] || { cat "$root/output" >&2; return 1; }
  else
    [[ "$status" != 0 ]] || { echo "unexpected success: $root" >&2; return 1; }
  fi
  local count=0
  if [[ -f "$root/installs" ]]; then count="$(wc -l <"$root/installs")"; fi
  [[ "$count" == "$expected_installs" ]] || {
    echo "unexpected install count $count: $root" >&2; return 1;
  }
  if [[ "$override" != none ]]; then
    [[ ! -f "$root/package-checks" ]] || { echo "override checked the package: $root" >&2; return 1; }
  fi
}

for entry in one-click e2e; do
  run_case "$entry" old none success 1 yes
  run_case "$entry" current none success 0 yes
  run_case "$entry" newer none success 0 yes
  run_case "$entry" old binary success 0 yes
  run_case "$entry" old none fail 1 no
  run_case "$entry" old none unchanged 1 no
done
run_case e2e old source success 0 yes
run_case e2e old skip success 0 yes
run_case e2e current none success 1 yes missing
run_case e2e current binary success 0 yes missing
printf '%s\n' 'Shelter package upgrade and explicit overrides: PASS'
