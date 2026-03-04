#!/bin/bash
# 05-configure-ssh.sh - Configure SSH to use only the injected host key
set -ex

echo "=== Configuring SSH host keys ==="

# Check if sshd_config exists
if [ ! -f /etc/ssh/sshd_config ]; then
    echo "Error: /etc/ssh/sshd_config not found"
    exit 1
fi

# Backup original config
cp /etc/ssh/sshd_config /etc/ssh/sshd_config.backup

# Comment out all existing HostKey directives
sed -i 's/^HostKey/#HostKey/g' /etc/ssh/sshd_config

# Add our injected host key
# The key will be placed by cai-secret-apply service from /run/cai/secrets/
cat >> /etc/ssh/sshd_config << 'EOF'

# Use only the injected host key from Trustee
HostKey /etc/ssh/ssh_host_rsa_key
EOF

echo "All existing HostKey directives have been commented out"
echo "Only using: HostKey /etc/ssh/ssh_host_rsa_key"
echo "=== SSH configuration updated ==="
