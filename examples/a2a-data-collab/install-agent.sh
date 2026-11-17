#!/bin/bash
set -euo pipefail

echo "installing A2A data collaboration LLM agent"

export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

# Configure the registry before bootstrapping Node with npm.
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

require_node22() {
    local n_bin node_version
    if command -v node >/dev/null 2>&1 &&
       node -e 'const [major] = process.versions.node.split(".").map(Number); process.exit(major >= 22 ? 0 : 1)' >/dev/null 2>&1; then
        return 0
    fi
    command -v npm >/dev/null 2>&1 || {
        echo "npm is required to install Node.js ${A2A_NODE_VERSION:-24.21.0}" >&2
        exit 1
    }
    command -v tar >/dev/null 2>&1 || {
        echo "tar is required to install Node.js ${A2A_NODE_VERSION:-24.21.0}" >&2
        exit 1
    }
    command -v xz >/dev/null 2>&1 || {
        echo "xz is required to install Node.js ${A2A_NODE_VERSION:-24.21.0}" >&2
        exit 1
    }
    if ! n_bin="$(resolve_n_bin)"; then
        npm install -g n --no-audit --no-fund
        hash -r
        n_bin="$(resolve_n_bin || true)"
    fi
    if [[ -z "$n_bin" ]]; then
        echo "n was installed but its executable could not be found" >&2
        exit 1
    fi
    if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
        install -d -m 0755 /usr/local/bin
        ln -sf "$n_bin" /usr/local/bin/n
        hash -r
    fi
    node_version="${A2A_NODE_VERSION:-24.21.0}"
    install_node_with_retry "$node_version"
    hash -r
    node -e 'const [major] = process.versions.node.split(".").map(Number); process.exit(major >= 22 ? 0 : 1)'
}

require_node22

install -d -m 0755 /usr/local/share/confidential-agent/a2a-data-collab
install -d -m 0755 /usr/local/bin /etc/cai /var/log
install -m 0755 /usr/local/share/confidential-agent/a2a-data-collab/agent-server.mjs /usr/local/bin/cai-a2a-llm-agent
touch /var/log/cai-a2a-data-collab.jsonl
chmod 0640 /var/log/cai-a2a-data-collab.jsonl || true

cat >/etc/systemd/system/cai-a2a-llm-agent.service <<'EOF'
[Unit]
Description=Confidential Agent A2A LLM collaboration demo
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
ExecStart=/usr/bin/env node /usr/local/bin/cai-a2a-llm-agent --config /etc/cai/a2a-agent.json
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-a2a-llm-agent.service
npm cache clean --force || true
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
