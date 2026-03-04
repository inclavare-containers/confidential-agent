#!/bin/bash
# 04-install-openclaw.sh - Install OpenClaw application
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing OpenClaw ==="

YUM_OPTS="--nogpgcheck"

# Install Node.js 22 (required for OpenClaw)
# Using nodesource repository for Node.js 22
curl -fsSL https://rpm.nodesource.com/setup_22.x | bash -
yum install -y $YUM_OPTS nodejs

# Verify Node.js version
node --version
npm --version

# Use Taobao NPM mirror
npm config set registry https://registry.npmmirror.com

# Fix npm git ssh issue when installing libsignal-node:
#     npm error command git --no-replace-objects ls-remote ssh://git@github.com/whiskeysockets/libsignal-node.git
git config --global url."https://gh-proxy.org/https://github.com/".insteadOf ssh://git@github.com/
git config --global url."https://gh-proxy.org/https://github.com//".insteadOf https://github.com/

# Install OpenClaw
npm install -g openclaw@latest

# Install OpenClaw dingtalk plugins
npm install -g pnpm@latest-10
pnpm config set registry https://registry.npmmirror.com/
mkdir -p /root/.openclaw/extensions/
pushd /root/.openclaw/extensions/
git clone https://github.com/soimy/openclaw-channel-dingtalk dingtalk
popd
pushd /root/.openclaw/extensions/dingtalk
pnpm install
popd

# Create OpenClaw configuration directory
mkdir -p /root/.openclaw

# Create cai-openclaw-gateway-launcher.service
# This is a wrapper service that manages OpenClaw gateway lifecycle
cat > /etc/systemd/system/cai-openclaw-gateway-launcher.service << 'EOF'
[Unit]
Description=CAI OpenClaw Gateway Launcher
After=network.target cai-secret-apply.service
Requires=cai-secret-apply.service

[Service]
Type=oneshot
RemainAfterExit=yes
User=root
WorkingDirectory=/root

# Key: Enable linger for root first, then install and start the OpenClaw gateway.
# This ensures the user-level systemd service created by 'openclaw gateway install'
# will persist and auto-start even without an active login session.
ExecStart=/bin/bash -c ' \
  loginctl enable-linger root && \
  export XDG_RUNTIME_DIR=/run/user/$(id -u) && \
  openclaw gateway install --force && \
  openclaw gateway start \
'

# Stop the gateway gracefully. Linger remains enabled (recommended for headless servers).
ExecStop=/bin/bash -c ' \
  export XDG_RUNTIME_DIR=/run/user/$(id -u) && \
  openclaw gateway stop \
'

StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-openclaw-gateway-launcher

echo "=== OpenClaw installation completed ==="
