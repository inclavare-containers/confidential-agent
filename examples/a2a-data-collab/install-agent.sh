#!/bin/bash
set -euo pipefail

echo "installing A2A data collaboration LLM agent"

export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

# Configure the registry before bootstrapping Node with npm.
npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"

source /usr/local/libexec/confidential-agent/node.sh


if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 22 ? 0 : 1)' >/dev/null 2>&1; then
    ensure_node "${A2A_NODE_VERSION:-24.21.0}"
fi

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
