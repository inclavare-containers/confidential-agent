#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release_dir="$(cd "${1:-$root/target/release-dist}" && pwd)"
image="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["container"])' "$root/packaging/release-build.json")"
# Share downloaded metadata/RPMs; the fresh container has its own RPM database.
docker run --rm -v "$release_dir:/release:ro" \
  -v "$root/target/ci-dnf-cache:/var/cache/dnf" \
  -v "$root/packaging/prepare-ci-host.sh:/prepare-ci-host.sh:ro" "$image" bash -euo pipefail -c '
  bash /prepare-ci-host.sh
  cd /release
  sha256sum -c SHA256SUMS
  bash install-rpm.sh --from-dir /release
  test "$(rpm -q --qf "%{VERSION}-%{RELEASE}" cagent)" = "0.2.0-1.al8"
  confidential-agent --version | grep -Fx "confidential-agent 0.2.0"
  for binary in confidential-agent confidential-agentd cai-gateway cai-pep; do
    if ldd "/usr/bin/$binary" | grep -F "not found"; then exit 1; fi
  done
  confidential-agent --help >/dev/null
  confidential-agentd --help >/dev/null
  # These two programs currently report usage with exit 1 for --help.
  if cai-gateway --help > /tmp/gateway-help 2>&1; then exit 1; fi
  grep -F "cai-gateway serve" /tmp/gateway-help
  if cai-pep --help > /tmp/pep-help 2>&1; then exit 1; fi
  grep -F "cai-pep attest collect-and-verify" /tmp/pep-help
  shelter --version
  /usr/libexec/cagent/bin/tng --version
  /usr/libexec/cagent/bin/attestation-challenge-client --help >/dev/null
  /usr/libexec/cagent/bin/cosign version >/dev/null
  /usr/libexec/cagent/bin/aliyun version >/dev/null
  /usr/libexec/cagent/bin/ossutil64 --version >/dev/null
  /usr/libexec/cagent/bin/rekor-cli version >/dev/null
  mkdir /consumer
  cd /consumer
  confidential-agent init allinone --stage build --non-interactive --output-dir /consumer/projects
  test -f /consumer/projects/allinone-builder/build.yaml
  confidential-agent spec validate --spec /consumer/projects/allinone-builder/build.yaml
  # The identical release is also safe to install again.
  bash /release/install-rpm.sh --from-dir /release
  echo "Clean Alinux3 installation, native tools, RPM templates and reinstall: PASS"
'
