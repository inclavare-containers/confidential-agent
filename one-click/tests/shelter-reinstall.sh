#!/usr/bin/env bash
# Stable release numbers still need to replace an older payload of the same EVR.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
touch "$fixture/shelter.rpm"
for entry in one-click e2e; do
  for fixture_installed in absent older same; do
    (
      if [[ "$entry" == one-click ]]; then
        source "$ROOT_DIR/one-click/lib/deploy.sh"
      else
        source "$ROOT_DIR/tools/e2e/lib/common.sh"
      fi
      log() { :; }
      record_cmd() { :; }
      is_root() { return 0; }
      assert_shelter_upgrade_idle() { :; }
      resolve_shelter_rpm() { printf '%s\n' "$fixture/shelter.rpm"; }
      rpm() {
        if [[ "$1" == -qp || "$fixture_installed" == same ]]; then
          printf '0:0.3.2-1.al8.x86_64'
        elif [[ "$fixture_installed" == older ]]; then
          printf '0:0.3.1-1.al8.x86_64'
        else
          return 1
        fi
      }
      package_manager() { printf dnf; }
      dnf() { printf '%s\n' "$*" >>"$fixture/$entry-$fixture_installed"; }
      if [[ "$entry" == one-click ]]; then
        install_shelter_rpm "$fixture/shelter.rpm"
      else
        install_bundled_shelter_rpm
      fi
    )
    operation=install
    [[ "$fixture_installed" != same ]] || operation=reinstall
    [[ "$(head -1 "$fixture/$entry-$fixture_installed")" == "$operation -y $fixture/shelter.rpm" ]]
    [[ "$(tail -1 "$fixture/$entry-$fixture_installed")" == 'install -y libguestfs-tools-c' ]]
  done
done
printf '%s\n' 'Stable-version Shelter payload reinstall: PASS'
