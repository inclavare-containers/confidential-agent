#!/bin/bash
# 02-install-attestation.sh - Install Attestation Agent and Confidential Data Hub
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing Attestation Agent and Confidential Data Hub ==="

YUM_OPTS="--nogpgcheck"

# Install attestation-agent (RPM package includes systemd service)
# Service file: /usr/lib/systemd/system/attestation-agent.service
yum install -y $YUM_OPTS attestation-agent

# Enable the service (will start on boot)
systemctl enable attestation-agent

# Install confidential-data-hub (includes binaries but no systemd service needed in this context)
yum install -y $YUM_OPTS confidential-data-hub

# Create necessary directories
mkdir -p /run/confidential-containers/attestation-agent

echo "=== Attestation Agent and Confidential Data Hub installation completed ==="
