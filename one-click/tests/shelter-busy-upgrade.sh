#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT_DIR/one-click/lib/deploy.sh"
fixture="$(mktemp -d)"
child=""
cleanup() {
  if [[ -n "$child" ]]; then kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi
  rm -rf "$fixture"
}
trap cleanup EXIT
die() { printf '%s\n' "$*" >&2; return 1; }
cp "$(command -v sleep)" "$fixture/shelter"
cp "$(command -v sleep)" "$fixture/unrelated"
assert_shelter_upgrade_idle "$fixture/absent" "$fixture/shelter"
"$fixture/shelter" 60 &
child=$!
for ((attempt=0; attempt<100; attempt++)); do
  [[ /proc/$child/exe -ef "$fixture/shelter" ]] && break
  sleep 0.01
done
[[ /proc/$child/exe -ef "$fixture/shelter" ]]
assert_shelter_upgrade_idle "$fixture/unrelated"
ln -s "$fixture/shelter" "$fixture/alias"
if assert_shelter_upgrade_idle "$fixture/alias" >"$fixture/out" 2>&1; then
  echo 'active Shelter executable incorrectly allowed an upgrade' >&2
  exit 1
fi
grep -Fq "running process $child" "$fixture/out"
kill "$child"
wait "$child" 2>/dev/null || true
child=""
assert_shelter_upgrade_idle "$fixture/shelter"
printf 'Shelter active-process upgrade protection: PASS\n'
