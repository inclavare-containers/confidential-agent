#!/usr/bin/env bash
# Exercise generated projects and the public lifecycle command grammar.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ca="${CA_TEST_BIN:-$root/target/debug/confidential-agent}"
export CA_ASSETS_DIR="$root"
export CA_PEP_BIN="${CA_PEP_BIN:-$root/target/debug/cai-pep}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
printf '{}\n' > "$work/release.json"
printf 'publisher fixture\n' > "$work/publisher.pem"
export DASHSCOPE_API_KEY=test-key
for harness in openclaw hermes codex claude-code opencode qodercli pi dsh; do
    "$ca" init allinone --stage deploy --non-interactive --service-id "instance-$harness" \
        --harness "$harness" --image-release "$work/release.json" --trusted-key "$work/publisher.pem" \
        --output-dir "$work/projects"
    "$ca" spec validate --spec "$work/projects/instance-$harness/instance.yaml"
done
"$ca" up --help > "$work/up-help"
"$ca" cleanup --help > "$work/cleanup-help"
"$ca" --state-dir "$work/state" cleanup --all --dry-run
if "$ca" cleanup > "$work/missing-target" 2>&1; then exit 1; fi
# Invalid input fails through the real parser before credentials/build/cloud calls.
printf 'schema: invalid\n' > "$work/invalid.yaml"
if "$ca" up --spec "$work/invalid.yaml" --non-interactive > "$work/invalid" 2>&1; then exit 1; fi
printf '%s\n' 'CLI harness templates, up and cleanup: PASS'
