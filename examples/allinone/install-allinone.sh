#!/bin/bash
set -euo pipefail
umask 022
export PATH="/usr/local/bin:$PATH"
export NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmmirror.com/}"
export UV_INDEX_URL="${UV_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple/}"
export PLAYWRIGHT_DOWNLOAD_HOST="${PLAYWRIGHT_DOWNLOAD_HOST:-https://npmmirror.com/mirrors/playwright}"

# The staged contract fixes both the install inputs and published version metadata.
contract=/usr/local/share/confidential-agent/image-runtime.json
export CODEX_VERSION="$(jq -er '.software_versions.codex' "$contract")"
export CLAUDE_CODE_VERSION="$(jq -er '.software_versions["claude-code"]' "$contract")"
export HERMES_COMMIT="$(jq -er '.software_versions.hermes' "$contract")"
export OPENCLAW_VERSION="$(jq -er '.software_versions.openclaw' "$contract")"
export DINGTALK_VERSION="$(jq -er '.software_versions.dingtalk' "$contract")"
QODER_VERSION="$(jq -er '.software_versions.qodercli' "$contract")"
OPENCODE_VERSION="$(jq -er '.software_versions.opencode' "$contract")"
PI_VERSION="$(jq -er '.software_versions.pi' "$contract")"
DSH_VERSION="$(jq -er '.software_versions.dsh' "$contract")"

cli=/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh
test -x /usr/local/bin/cai-pep
test -s /opt/data/allinone/home/.agents/skills/tdx-remote-attestation/SKILL.md
(
    source /usr/local/libexec/confidential-agent/software-versions.sh
    node_version="$(ca_software_version node)"
    node_sha256="$(ca_software_version node_linux_x64_sha256)"
    node_archive="node-v${node_version}-linux-x64.tar.xz"
    node_work="$(mktemp -d)"
    trap 'rm -rf -- "$node_work"' EXIT
    curl -fsSL "${N_NODE_MIRROR:-https://npmmirror.com/mirrors/node}/v${node_version}/${node_archive}" \
        -o "$node_work/$node_archive"
    printf '%s  %s\n' "$node_sha256" "$node_work/$node_archive" | sha256sum --check
    tar -xJf "$node_work/$node_archive" -C /usr/local --strip-components=1 \
        --no-same-owner --no-overwrite-dir
)
"$cli" codex
"$cli" claude-code
install -d -m 0755 /opt/data/allinone
install -d -m 0700 /opt/data/allinone/home
HERMES_HOME=/opt/data/allinone/hermes HERMES_COMPACT_RUNTIME=1 HERMES_PREBUILT_RELEASE=1 \
    /usr/local/libexec/confidential-agent/hermes/install-hermes-agent-runtime.sh
npm config set registry "$NPM_REGISTRY"
npm install -g --no-audit --no-fund \
    "openclaw@$OPENCLAW_VERSION" \
    "@qoder-ai/qodercli@$QODER_VERSION" \
    "opencode-ai@$OPENCODE_VERSION"
npm install -g --no-audit --no-fund \
    --allow-scripts=@deepseek-ai/dsh-subprocess-local,koffi,node-pty,@google/genai,protobufjs \
    "@earendil-works/pi-coding-agent@$PI_VERSION" \
    "@deepseek-ai/dsh@$DSH_VERSION"

# Only OpenClaw has the executor patch. Other harnesses use cai-pep's local
# attestation subcommand directly, without a policy server or execution hooks.
# DingTalk and CAI plugins are prebuilt in the shared image; openclaw.json
# controls per-instance plugin activation and --disable-pep.
pep=/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh
claw_home=/opt/data/allinone/home/.openclaw
CA_DISABLE_PEP=0 "$pep" setup-runtime root "$claw_home"
CA_DISABLE_PEP=0 /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh \
    root /opt/data/allinone/home
test -f "$claw_home/extensions/dingtalk/dist/index.js"
test -d "$claw_home/extensions/cai-a2a"
test -d "$claw_home/extensions/cai-pep"
install -d -m 0700 /opt/data/allinone/workspace
policy=/etc/cai/pep/policy.json
jq '.workspace_host_path = "/opt/data/allinone/workspace"
    | .allowed_workspace_prefixes = ["/opt/data/allinone/workspace"]
    | .denied_path_prefixes += ["/opt/data/allinone/home", "/opt/data/allinone/hermes"]' \
    "$policy" > "$policy.new"
install -m 0640 -o root -g openclaw "$policy.new" "$policy"
rm -f "$policy.new"
# cai-agent starts this service only when OpenClaw is selected, after resources
# arrive. Do not let preset-all start it for Codex, Claude, Hermes, etc.
systemctl disable cai-pep.service

# Current Shelter preserves ELF payloads. Select the harness only after CA has
# delivered this instance's resources; individual installers enable their units.
install -d -m 0755 /etc/systemd/system-preset
cat > /etc/systemd/system-preset/00-allinone.preset <<'PRESET'
disable cai-codex-app-server.service
disable cai-hermes-agent.service
disable cai-pep.service
disable sshd.service
disable sshd.socket
enable cai-ssh.path
PRESET
# mkosi applies preset-all after post-install scripts and would otherwise
# re-enable the distribution SSH server before instance credentials arrive.
for unit in cai-codex-app-server.service cai-hermes-agent.service sshd.service sshd.socket; do
    if [[ -f /etc/systemd/system/$unit || -f /usr/lib/systemd/system/$unit ]]; then
        systemctl disable "$unit"
    fi
done
rm -f /etc/ssh/ssh_host_* /root/.ssh/authorized_keys
systemctl enable cai-ssh.path
install -d -m 0755 /usr/local/share/confidential-agent
{
    npm list -g --depth=0 --json
    sed -n 's/^commit=//p' /usr/local/share/confidential-agent/hermes-install.txt
    jq -er '.version' "$claw_home/extensions/dingtalk/package.json"
} > /usr/local/share/confidential-agent/allinone-versions.txt
python3.11 /usr/local/libexec/confidential-agent/allinone/finalize-runtime.py
for command in openclaw hermes qoder opencode codex claude pi dsh; do
    "$command" --version
done
