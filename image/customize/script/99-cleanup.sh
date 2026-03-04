#!/bin/bash
# 99-cleanup.sh - Clean up the image before snapshot
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Cleaning up image ==="

# Clean yum cache
yum clean all
rm -rf /var/cache/yum

# Clear bash history
history -c
rm -f /root/.bash_history
rm -f /home/*/.bash_history 2>/dev/null || true

# Remove temporary files
rm -rf /tmp/*
rm -rf /var/tmp/*

# Remove SSH host keys (will be regenerated on first boot)
rm -f /etc/ssh/ssh_host_*

# Clear machine-id (will be regenerated on first boot)
> /etc/machine-id

# Remove cloud-init artifacts
rm -rf /var/lib/cloud/instances/*

# Sync filesystem
sync

echo "=== Cleanup completed ==="
