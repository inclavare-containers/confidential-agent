#!/usr/bin/env bash
# Source-checkout installation; production users install cagent with the RPM manager.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ "$EUID" == 0 ]] || { echo 'Installing native tools requires root; alternatively install the cagent RPM' >&2; exit 1; }
destination="${CA_TOOLS_DIR:-/usr/libexec/cagent/bin}"
pm="$(command -v dnf || command -v yum)"
"$pm" install -y attestation-challenge-client libtdx-verify trusted-network-gateway-2.9.2-1.al8
"$root/packaging/fetch-native-tools.sh" "$destination"
[[ -x /usr/libexec/shelter/bin/rekor-cli ]] || { echo 'Install Shelter 0.3.2-1 or newer for rekor-cli' >&2; exit 1; }
for tool in attestation-challenge-client tng; do
  ln -sfn "/usr/bin/$tool" "$destination/$tool"
done
ln -sfn /usr/libexec/shelter/bin/rekor-cli "$destination/rekor-cli"
"$destination/tng" --version
"$destination/oras" version >/dev/null
"$destination/attestation-challenge-client" --help >/dev/null
