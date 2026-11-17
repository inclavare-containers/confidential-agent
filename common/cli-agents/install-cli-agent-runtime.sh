#!/bin/bash
set -euo pipefail
if [[ -f /usr/local/libexec/confidential-agent/software-versions.sh ]]; then
    source /usr/local/libexec/confidential-agent/software-versions.sh
elif [[ -n "${ROOT_DIR:-}" && -f "$ROOT_DIR/common/software-versions.sh" ]]; then
    source "$ROOT_DIR/common/software-versions.sh"
else
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/software-versions.sh"
fi

AGENT="${1:?usage: install-cli-agent-runtime.sh <claude-code|codex>}"
NODE_VERSION="${CLI_AGENT_NODE_VERSION:-$(ca_software_version node)}"
FINALIZE_REPAIR="${CLI_AGENT_FINALIZE_REPAIR:-0}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

# Interactive Codex manages a background app-server using procps-compatible ps.
# Check before downloads; --version and `codex exec` do not exercise this path.
if [[ "$AGENT" == "codex" ]]; then
    if ! command -v ps >/dev/null 2>&1 ||
       ! ps --version >/dev/null; then
        echo "Codex requires procps-ng (ps); add procps-ng to build.packages" >&2
        exit 1
    fi
fi

npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"

source /usr/local/libexec/confidential-agent/node.sh


install_package() {
    local package="$1"
    local bin_name="$2"
    npm install -g "$package" --force --no-audit --no-fund
    hash -r
    local installed_bin
    installed_bin="$(command -v "$bin_name" 2>/dev/null || true)"
    if [[ -z "$installed_bin" ]]; then
        echo "$bin_name binary was not installed by $package" >&2
        exit 1
    fi
    if [[ "$installed_bin" != "/usr/local/bin/$bin_name" ]]; then
        ln -sf "$installed_bin" "/usr/local/bin/$bin_name"
    fi
    chmod a+rx "$installed_bin" "/usr/local/bin/$bin_name" || true
}

quote_env_value() {
    printf '%q' "$1"
}

write_finalize_repair_script() {
    local script="/shelter-finalize.d/99-${AGENT}-cli-agent-runtime.sh"

    if [[ "$FINALIZE_REPAIR" == "1" ]]; then
        return 0
    fi

    install -d -m 0755 /shelter-finalize.d
    {
        printf '#!/bin/bash\n'
        printf 'set -euo pipefail\n'
        printf 'export CLI_AGENT_FINALIZE_REPAIR=1\n'
        printf 'export CLI_AGENT_NODE_VERSION=%s\n' "$(quote_env_value "$NODE_VERSION")"
        printf 'export NPM_REGISTRY=%s\n' "$(quote_env_value "${NPM_REGISTRY:-https://registry.npmmirror.com/}")"
        if [[ -n "${CLAUDE_CODE_VERSION:-}" ]]; then
            printf 'export CLAUDE_CODE_VERSION=%s\n' "$(quote_env_value "$CLAUDE_CODE_VERSION")"
        fi
        if [[ -n "${CODEX_VERSION:-}" ]]; then
            printf 'export CODEX_VERSION=%s\n' "$(quote_env_value "$CODEX_VERSION")"
        fi
        printf '/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh %s\n' "$(quote_env_value "$AGENT")"
    } >"$script"
    chmod 0755 "$script"
}

install_claude_code() {
    install -d -m 0700 /root/.claude /root/.claude/skills
    install -d -m 0775 /workspace
    git -C /workspace init -q || true
    local version="${CLAUDE_CODE_VERSION:-$(ca_software_version claude-code)}"
    # Shelter's image cleanup strips ELF files during finalize. Claude Code's
    # Bun standalone binary stores its payload after the ELF sections, so it
    # must be reinstalled after that cleanup step.
    if [[ "$FINALIZE_REPAIR" == "1" ]]; then
        npm uninstall -g @anthropic-ai/claude-code >/dev/null 2>&1 || true
    fi
    install_package "@anthropic-ai/claude-code@$version" claude
    local version_output
    version_output="$(claude --version)"
    if [[ "$version_output" != *"Claude Code"* ]]; then
        echo "unexpected Claude Code version output: $version_output" >&2
        exit 1
    fi
    printf '%s\n' "$version_output" >/usr/local/share/confidential-agent-claude-version.txt
}

install_codex() {
    install -d -m 0700 /root/.codex /root/.codex/sessions /root/.config/confidential-agent/codex
    install -d -m 0755 /root/.agents /root/.agents/skills
    install -d -m 0775 /workspace
    git -C /workspace init -q || true
    local version="${CODEX_VERSION:-$(ca_software_version codex)}"
    if [[ "$FINALIZE_REPAIR" == "1" ]]; then
        npm uninstall -g @openai/codex >/dev/null 2>&1 || true
    fi
    install_package "@openai/codex@$version" codex
    local version_output
    version_output="$(codex --version)"
    if [[ "$version_output" != codex* ]]; then
        echo "unexpected Codex version output: $version_output" >&2
        exit 1
    fi
    printf '%s\n' "$version_output" >/usr/local/share/confidential-agent-codex-version.txt

    cat >/usr/local/bin/cai-codex-wait-config <<'EOF'
#!/bin/bash
set -euo pipefail
for _ in $(seq 1 180); do
    if [[ -s /root/.codex/config.toml &&
          -s /root/.config/confidential-agent/codex/codex.env &&
          -s /root/.codex/app-server-token ]]; then
        grep -Eq '^OPENAI_API_KEY=.+' /root/.config/confidential-agent/codex/codex.env && exit 0
    fi
    sleep 2
done
echo "Codex config, API key environment, or app-server token did not become ready" >&2
exit 1
EOF
    chmod 0755 /usr/local/bin/cai-codex-wait-config

    cat >/etc/profile.d/cai-codex.sh <<'EOF'
if [ "$(id -u)" = "0" ] && [ -f /root/.config/confidential-agent/codex/codex.env ]; then
    set -a
    # shellcheck disable=SC1091
    . /root/.config/confidential-agent/codex/codex.env
    set +a
fi
EOF
    chmod 0644 /etc/profile.d/cai-codex.sh

    cat >/etc/systemd/system/cai-codex-app-server.service <<'EOF'
[Unit]
Description=Codex app-server for Confidential Agent
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
WorkingDirectory=/workspace
Environment=HOME=/root
Environment=CODEX_HOME=/root/.codex
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
EnvironmentFile=-/root/.config/confidential-agent/codex/codex.env
ExecStartPre=/usr/local/bin/cai-codex-wait-config
ExecStart=/usr/local/bin/codex app-server --listen ws://0.0.0.0:4500 --ws-auth capability-token --ws-token-file /root/.codex/app-server-token
Restart=always
RestartSec=5
TimeoutStartSec=600
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload || true
    systemctl enable cai-codex-app-server.service
}

ensure_node "$NODE_VERSION"
case "$AGENT" in
    claude-code) install_claude_code ;;
    codex) install_codex ;;
    *) echo "unsupported CLI agent: $AGENT" >&2; exit 1 ;;
esac
write_finalize_repair_script

npm cache clean --force || true
npm config delete registry || true
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
