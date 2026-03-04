#!/bin/bash
# 03-install-tng.sh - Install Trusted Network Gateway
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing Trusted Network Gateway ==="

YUM_OPTS="--nogpgcheck"

# Install TNG (RPM package includes systemd service)
# Service file: /usr/lib/systemd/system/trusted-network-gateway.service
yum install -y $YUM_OPTS trusted-network-gateway

# Create TNG configuration directory
mkdir -p /etc/tng

# Deploy TNG configuration
cat <<EOF > /etc/tng/config.json
{
  "control_interface": {
    "restful": {
      "host": "127.0.0.1",
      "port": 50000
    }
  },
  "add_egress": [
    {
      "netfilter": {
        "capture_dst": {
          "port": 18789
        }
      },
      "attest": {
        "model": "background_check",
        "aa_addr": "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock"
      }
    }
  ]
}
EOF

# Enable the service (will start on boot)
systemctl enable trusted-network-gateway

echo "=== Trusted Network Gateway installation completed ==="
