#!/usr/bin/env bash
# Cleanup must work without build tools and must not replace the installed CLI.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

cat > "$work/bin/shelter" <<'SHELTER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$TEST_WORK/shelter-calls"
case "$*" in
  'rm --help')
    [[ "$TEST_FAULT" == removal ]] || printf '%s\n' '  -f, --force'
    ;;
  'network update --help')
    [[ "$TEST_FAULT" == network ]] || printf '%s\n' '  -c, --config <FILE>'
    ;;
  '--version') printf '%s\n' 'shelter 0.3.2' ;;
  *) echo "unexpected build command: $*" >&2; exit 97 ;;
esac
SHELTER
cat > "$work/bin/cryptpilot-fde-host" <<'CRYPTPILOT'
#!/usr/bin/env bash
printf '%s\n' called >> "$TEST_WORK/cryptpilot-calls"
printf '%s\n' 'cryptpilot-fde-host 0.9.4'
CRYPTPILOT
cat > "$work/runner" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$TEST_REPO"
source "$ROOT_DIR/one-click/lib/deploy.sh"
die() { echo "$*" >&2; exit 1; }
log() { :; }
resolve_shelter_rpm() { echo 'cleanup tried to resolve an upgrade' >&2; exit 98; }
install_shelter_rpm() { echo 'cleanup tried to install an upgrade' >&2; exit 99; }
if [[ "$TEST_FAULT" == binary ]]; then
  CA_SHELTER_BIN="$TEST_WORK/missing-shelter"
elif [[ "$TEST_OVERRIDE" == yes ]]; then
  CA_SHELTER_BIN="$TEST_WORK/bin/shelter"
fi
ensure_shelter
RUNNER
chmod +x "$work/bin/"* "$work/runner"

for mode in cleanup cleanup-openclaw-vllm cleanup-vllm-inference; do
  for override in no yes; do
    for fault in none removal network binary; do
      rm -f "$work/shelter-calls" "$work/cryptpilot-calls"
      status=0
      env -u CA_SHELTER_BIN PATH="$work/bin:$PATH" CA_MODE="$mode" \
        TEST_REPO="$repo" TEST_WORK="$work" TEST_FAULT="$fault" TEST_OVERRIDE="$override" \
        bash "$work/runner" > "$work/output" 2>&1 || status=$?
      if [[ "$fault" == none ]]; then
        [[ "$status" == 0 ]] || { cat "$work/output" >&2; exit 1; }
        printf '%s\n' 'rm --help' 'network update --help' '--version' > "$work/expected"
        cmp "$work/expected" "$work/shelter-calls"
      else
        [[ "$status" != 0 ]] || { echo "cleanup accepted missing capability: $fault" >&2; exit 1; }
      fi
      [[ ! -e "$work/cryptpilot-calls" ]] || { echo 'cleanup invoked Cryptpilot' >&2; exit 1; }
      if grep -Eq 'tried to (resolve|install) an upgrade' "$work/output"; then
        cat "$work/output" >&2
        exit 1
      fi
    done
  done
done

# Build/deploy keeps the existing pinned Cryptpilot contract.
if PATH="$work/bin:$PATH" TEST_WORK="$work" \
    bash "$repo/hack/check-shelter.sh" "$work/bin/shelter" > "$work/build-output" 2>&1; then
  echo 'build preflight accepted an unsupported Cryptpilot version' >&2
  exit 1
fi
[[ -s "$work/cryptpilot-calls" ]]
grep -Fq 'Cryptpilot host 0.9.3' "$work/build-output"
printf '%s\n' 'Shelter cleanup capability checks and unchanged build version gate: PASS'
