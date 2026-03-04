#!/bin/bash
# 02-configure-grub.sh - Configure GRUB cmdline for serial console
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Configuring GRUB cmdline ==="

# Configure GRUB cmdline: remove quiet, add serial console
GRUB_CMDLINE="console=tty0 console=ttyS0,115200n8"

# Update GRUB configuration
if [ -f /etc/default/grub ]; then
    # Remove 'quiet' from GRUB_CMDLINE_LINUX if present
    sed -i 's/quiet//' /etc/default/grub
    
    # Check if console is already configured
    if grep -q "GRUB_CMDLINE_LINUX=" /etc/default/grub; then
        # Check if console is already present
        if ! grep "GRUB_CMDLINE_LINUX=" /etc/default/grub | grep -q "console="; then
            # No console configured, add both tty0 and ttyS0
            sed -i "s/GRUB_CMDLINE_LINUX=\"/GRUB_CMDLINE_LINUX=\"${GRUB_CMDLINE} /" /etc/default/grub
        fi
    else
        # Add new line with console configuration
        echo "GRUB_CMDLINE_LINUX=\"${GRUB_CMDLINE}\"" >> /etc/default/grub
    fi
    
    # Regenerate GRUB configuration
    if command -v grub2-mkconfig &> /dev/null; then
        grub2-mkconfig -o /boot/grub2/grub.cfg
    elif command -v grub-mkconfig &> /dev/null; then
        grub-mkconfig -o /boot/grub/grub.cfg
    fi
fi

echo "=== GRUB cmdline configuration completed ==="
