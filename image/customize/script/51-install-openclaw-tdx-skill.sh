#!/bin/bash
# 51-install-openclaw-tdx-skill.sh - Install TDX attestation tools and register skill for OpenClaw TDX awareness
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing OpenClaw TDX awareness components ==="

YUM_OPTS="--nogpgcheck"

# Install attestation tools for TDX remote attestation
yum install -y attestation-challenge-client trustiflux-api-server || { echo "Error: Failed to install attestation packages"; exit 1; }

# Enable trustiflux-api-server service
systemctl enable trustiflux-api-server || { echo "Error: Failed to enable trustiflux-api-server"; exit 1; }

# Create OpenClaw skills directory if not exists
mkdir -p /root/.openclaw/skills

# Copy TDX skill to OpenClaw skills directory
mkdir -p /root/.openclaw/skills/tdx-remote-attestation/
cp "${SCRIPT_DIR}/../files/skill.md" /root/.openclaw/skills/tdx-remote-attestation/SKILL.md
echo "✅ TDX remote attestation skill registered to OpenClaw"

echo "=== OpenClaw TDX awareness components installed ==="
