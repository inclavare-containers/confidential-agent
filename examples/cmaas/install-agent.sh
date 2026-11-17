#!/bin/bash
set -euo pipefail

echo "installing CMaaS demo agent"

export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

# Configure the registry before bootstrapping Node with npm.
npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"

source /usr/local/libexec/confidential-agent/node.sh


if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 22 ? 0 : 1)' >/dev/null 2>&1; then
    ensure_node "${CMAAS_NODE_VERSION:-24.21.0}"
fi
install -d -m 0755 /usr/local/share/confidential-agent/cmaas
install -m 0755 /usr/local/share/confidential-agent/cmaas/agent-client.mjs /usr/local/bin/cmaas-agent-client

cat >/usr/local/share/confidential-agent/cmaas/agent-health.mjs <<'EOF'
#!/usr/bin/env node
import http from "node:http";

const server = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ ok: true, service: "cmaas-agent" }) + "\n");
});

server.listen(18080, "0.0.0.0", () => {
  console.log("cmaas-agent health service listening on 0.0.0.0:18080");
});
EOF
chmod 0755 /usr/local/share/confidential-agent/cmaas/agent-health.mjs

cat >/etc/systemd/system/cai-cmaas-agent.service <<'EOF'
[Unit]
Description=CMaaS demo agent health service
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
ExecStart=/usr/bin/env node /usr/local/share/confidential-agent/cmaas/agent-health.mjs
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-cmaas-agent.service
npm cache clean --force || true
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
