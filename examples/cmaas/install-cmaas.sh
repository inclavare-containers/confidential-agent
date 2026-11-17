#!/bin/bash
set -euo pipefail

echo "installing CMaaS memory service"

MCP_PROXY_VERSION="${MCP_PROXY_VERSION:-6.5.0}"
MCP_MEMORY_VERSION="${MCP_MEMORY_VERSION:-2026.1.26}"
CMAAS_NODE_PREFIX="${CMAAS_NODE_PREFIX:-/opt/confidential-agent/cmaas-node}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

# Configure the registry before bootstrapping Node with npm.
npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"

source /usr/local/libexec/confidential-agent/node.sh


if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 22 ? 0 : 1)' >/dev/null 2>&1; then
    ensure_node "${CMAAS_NODE_VERSION:-24.21.0}"
fi

install -d -m 0755 /var/lib/mcp-memory
install -d -m 0755 /usr/local/share/confidential-agent/cmaas
install -d -m 0755 /usr/local/bin
install -d -m 0755 "$CMAAS_NODE_PREFIX"

prepare_cmaas_node_prefix() {
    cd "$CMAAS_NODE_PREFIX"
    npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"
    if [[ ! -f package.json ]]; then
        npm init -y
    fi
    npm install \
        --omit=dev \
        --no-audit \
        --no-fund \
        "mcp-proxy@${MCP_PROXY_VERSION}" \
        "@modelcontextprotocol/server-memory@${MCP_MEMORY_VERSION}"
}

MCP_PROXY_JS="${CMAAS_NODE_PREFIX}/node_modules/mcp-proxy/dist/bin/mcp-proxy.mjs"
MCP_MEMORY_JS="${CMAAS_NODE_PREFIX}/node_modules/@modelcontextprotocol/server-memory/dist/index.js"
if [[ ! -f "$MCP_PROXY_JS" || ! -f "$MCP_MEMORY_JS" ]]; then
    prepare_cmaas_node_prefix
fi
test -f "$MCP_PROXY_JS"
test -f "$MCP_MEMORY_JS"

cat >/usr/local/bin/cmaas-mcp-proxy <<EOF
#!/bin/bash
exec /usr/bin/env node "$MCP_PROXY_JS" "\$@"
EOF
cat >/usr/local/bin/cmaas-mcp-memory <<EOF
#!/bin/bash
exec /usr/bin/env node "$MCP_MEMORY_JS" "\$@"
EOF
chmod 0755 /usr/local/bin/cmaas-mcp-proxy /usr/local/bin/cmaas-mcp-memory

cat >/etc/systemd/system/cai-cmaas-mcp-proxy.service <<EOF
[Unit]
Description=CMaaS MCP memory stdio-to-streamable-http proxy
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=MEMORY_FILE_PATH=/var/lib/mcp-memory/memory.jsonl
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
ExecStart=/usr/local/bin/cmaas-mcp-proxy --host 127.0.0.1 --port 8000 --server stream --streamEndpoint /mcp -- /usr/local/bin/cmaas-mcp-memory
Restart=always
RestartSec=5
TimeoutStartSec=120
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-cmaas-mcp-proxy.service
npm cache clean --force || true
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
