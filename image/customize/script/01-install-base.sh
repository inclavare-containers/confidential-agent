#!/bin/bash
# 01-install-base.sh - Install base packages and configure system
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing base packages ==="

YUM_OPTS="--nogpgcheck"

# Install essential tools
yum install -y $YUM_OPTS \
    cmake \
    gcc-c++ \
    curl \
    wget \
    jq \
    openssl \
    vim \
    net-tools \
    bind-utils \
    tar \
    gzip \
    git

echo "=== Base packages installation completed ==="
