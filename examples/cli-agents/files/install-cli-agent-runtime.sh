#!/bin/bash
set -euo pipefail
if [[ -f /usr/local/libexec/confidential-agent/software-versions.sh ]]; then
    source /usr/local/libexec/confidential-agent/software-versions.sh
elif [[ -n "${ROOT_DIR:-}" && -f "$ROOT_DIR/common/software-versions.sh" ]]; then
    source "$ROOT_DIR/common/software-versions.sh"
else
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/common/software-versions.sh"
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

resolve_n_bin() {
    local candidate npm_prefix npm_root
    candidate="$(command -v n 2>/dev/null || true)"
    if [[ -n "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi
    npm_prefix="$(npm prefix -g 2>/dev/null || true)"
    npm_root="$(npm root -g 2>/dev/null || true)"
    for candidate in \
        "$npm_prefix/bin/n" \
        "$npm_root/n/bin/n" \
        /usr/local/bin/n \
        /usr/bin/n; do
        if [[ -f "$candidate" ]]; then
            chmod 0755 "$candidate" || true
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    candidate="$(find /usr/local /usr -path '*/node_modules/n/bin/n' -type f -print -quit 2>/dev/null || true)"
    if [[ -n "$candidate" ]]; then
        chmod 0755 "$candidate" || true
        printf '%s\n' "$candidate"
        return 0
    fi
    return 1
}

install_node_with_retry() {
    local node_version="$1"
    local attempt delay mirror mirrors timeout_sec
    timeout_sec="${NODE_INSTALL_TIMEOUT_SEC:-300}"
    if [[ -n "${N_NODE_MIRROR:-}" ]]; then
        mirrors=("$N_NODE_MIRROR")
    else
        mirrors=("https://npmmirror.com/mirrors/node" "https://nodejs.org/dist")
    fi
    for mirror in "${mirrors[@]}"; do
        export N_NODE_MIRROR="$mirror"
        for attempt in 1 2 3; do
            rm -rf "/usr/local/n/versions/node/$node_version"
            if command -v timeout >/dev/null 2>&1; then
                timeout "$timeout_sec" n "$node_version" && return 0
            else
                n "$node_version" && return 0
            fi
            delay=$((attempt * 15))
            echo "Node.js $node_version install attempt $attempt from $mirror failed; retrying in ${delay}s" >&2
            sleep "$delay"
        done
    done
    echo "failed to install Node.js $node_version after trying configured mirrors" >&2
    return 1
}

ensure_node22() {
    local n_bin
    if command -v node >/dev/null 2>&1 &&
       [[ "$(node --version)" == "v$NODE_VERSION" ]] >/dev/null 2>&1; then
        return 0
    fi
    command -v npm >/dev/null 2>&1 || {
        echo "npm is required to install Node.js $NODE_VERSION" >&2
        exit 1
    }
    command -v tar >/dev/null 2>&1 || {
        echo "tar is required to install Node.js $NODE_VERSION" >&2
        exit 1
    }
    command -v xz >/dev/null 2>&1 || {
        echo "xz is required to install Node.js $NODE_VERSION" >&2
        exit 1
    }
    if ! n_bin="$(resolve_n_bin)"; then
        npm install -g n --no-audit --no-fund
        hash -r
        n_bin="$(resolve_n_bin || true)"
    fi
    if [[ -z "$n_bin" ]]; then
        echo "n was installed but its executable could not be found; npm prefix=$(npm prefix -g 2>/dev/null || true), npm root=$(npm root -g 2>/dev/null || true)" >&2
        exit 1
    fi
    if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
        install -d -m 0755 /usr/local/bin
        ln -sf "$n_bin" /usr/local/bin/n
        hash -r
    fi
    install_node_with_retry "$NODE_VERSION"
    export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
    hash -r
    [[ "$(node --version)" == "v$NODE_VERSION" ]]
}

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

ensure_node22
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
