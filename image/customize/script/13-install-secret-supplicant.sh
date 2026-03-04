#!/bin/bash
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Configuring CAI secret management services ==="

# Create CAI secret management directories
mkdir -p /opt/cai/secrets
mkdir -p /run/confidential-containers
mkdir -p /run/cai/secrets

# Create CDH config
cat > /opt/cai/secrets/cdh-config.toml << 'CONFIG_EOF'
socket = "unix:///run/confidential-containers/cdh.sock"
[kbc]
name = "cc_kbc"
url = "__TRUSTEE_URL__"
CONFIG_EOF

# Replace placeholder
[ -z "$TRUSTEE_URL" ] && { echo "Error: TRUSTEE_URL is required"; exit 1; }
sed -i "s|__TRUSTEE_URL__|$TRUSTEE_URL|" /opt/cai/secrets/cdh-config.toml

# Create the cai-secret-fetch script (runs in initrd, stores secrets to /run/cai/secrets)
cat > /opt/cai/secrets/cai-secret-fetch << 'SCRIPT_EOF'
#!/bin/bash
set -e

CONFIG_FILE="/opt/cai/secrets/cdh-config.toml"

# Sync system time from HTTP response header (no NTP daemon required)
# Uses Alibaba Cloud internal endpoint (100.100.100.200) which is always reachable from ECS
sync_time_from_http() {
    echo "Attempting time sync from HTTP server..."
    
    local http_endpoints=("100.100.100.200" "www.baidu.com")
    local remote_time
    local http_endpoint
    
    # Try each endpoint until one succeeds
    for http_endpoint in "${http_endpoints[@]}"; do
        remote_time=$(curl -sI --connect-timeout 5 "http://${http_endpoint}" 2>/dev/null | grep -i "^date:" | sed 's/date:\s*//i')
        if [ -n "${remote_time}" ]; then
            break
        fi
    done
    
    if [ -n "${remote_time}" ]; then
        echo "  Retrieved time from HTTP server (${http_endpoint}): ${remote_time}"
        
        if date -s "${remote_time}" >/dev/null 2>&1; then
            echo "  System time updated to: $(date)"
            echo "Time synchronization completed successfully"
            return 0
        else
            echo "  Failed to set system time"
        fi
    else
        echo "  Failed to connect to any HTTP server"
    fi
    
    echo "Warning: Time sync failed, system time may be inaccurate"
    return 1
}

# Sync time before fetching secrets (attestation requires accurate time)
echo "=== Syncing system time ==="
sync_time_from_http
echo ""
STAGE_DIR="/run/cai/secrets"

# Define secret URIs and target paths (format: key_uri:target_path)
SECRETS=(
    "kbs:///default/local-resources/disk_passphrase:disk_key"
    "kbs:///default/local-resources/sshd_server_key:ssh_host_rsa_key"
    "kbs:///default/local-resources/sshd_server_key.pub:ssh_host_rsa_key.pub"
    "kbs:///default/local-resources/openclaw_config:openclaw.json"
)

# Create staging directory
mkdir -p "$STAGE_DIR"

# Fetch secrets using cdh
CDH_BIN="/usr/bin/confidential-data-hub"

for entry in "${SECRETS[@]}"; do
    # Use parameter expansion to properly split at the last colon
    # key_uri gets everything before the last colon (including kbs:// protocol)
    key_uri="${entry%:*}"
    # filename gets everything after the last colon
    filename="${entry##*:}"
    output_file="$STAGE_DIR/$filename"
    
    echo ""
    echo "========================================"
    echo "Fetching secret: $key_uri -> $output_file"
    
    # Create temp file with .tmp suffix (initrd environment lacks mktemp)
    temp_file="${output_file}.tmp"

    # Clean up any existing temp file first
    rm -f "$temp_file"

    # Fetch secret from KBS (show output directly for debugging)
    if $CDH_BIN -c "$CONFIG_FILE" get-resource --resource-uri "$key_uri" > "$temp_file.b64"; then
        # Decode base64 content
        if base64 -d "$temp_file.b64" > "$temp_file" 2>/dev/null; then
            # Move temp file to final location only on success
            mv "$temp_file" "$output_file"
            chmod 600 "$output_file"
            rm -f "$temp_file.b64"
            echo "✓ Secret staged: $output_file"
        else
            rm -f "$temp_file" "$temp_file.b64"
            echo "✗ Error: Failed to decode secret $key_uri (invalid base64)" >&2
            exit 1
        fi
    else
        rm -f "$temp_file.b64"
        echo "✗ Error: Failed to fetch secret $key_uri" >&2
        exit 1
    fi
done

echo ""
echo "========================================"

echo "=== Secrets staged to $STAGE_DIR ==="
SCRIPT_EOF

chmod +x /opt/cai/secrets/cai-secret-fetch

# Create cai-secret-apply.service (runs in normal system, moves secrets to final location)
cat > /etc/systemd/system/cai-secret-apply.service << 'EOF'
[Unit]
Description=CAI Secret Apply - Move staged secrets to final location
# If sshd is installed, run before it to ensure SSH keys are ready
# If sshd is not installed (hardened image), this ordering is ignored
Before=sshd.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/opt/cai/secrets/cai-secret-apply
StandardOutput=journal+console
StandardError=journal+console
# Fail fast - if any secret preparation fails, the service should fail
Restart=no
SuccessExitStatus=0

[Install]
WantedBy=multi-user.target
EOF

# Create cai-secret-apply script
cat > /opt/cai/secrets/cai-secret-apply << 'SCRIPT_EOF'
#!/bin/bash
set -e

STAGE_DIR="/run/cai/secrets"
FINAL_LOCATIONS=(
    "ssh_host_rsa_key:/etc/ssh/ssh_host_rsa_key"
    "ssh_host_rsa_key.pub:/etc/ssh/ssh_host_rsa_key.pub"
    "openclaw.json:/root/.openclaw/openclaw.json"
)

# Create final directories
mkdir -p /etc/luks-keys
mkdir -p /etc/ssh
mkdir -p /root/.ssh
mkdir -p /root/.openclaw
chmod 700 /root/.ssh
chmod 700 /root/.openclaw

# Move secrets to final locations
for entry in "${FINAL_LOCATIONS[@]}"; do
    IFS=':' read -r filename final_path <<< "$entry"
    source_file="$STAGE_DIR/$filename"

    if [ -f "$source_file" ]; then
        cp "$source_file" "$final_path"

        # Set appropriate permissions
        if [[ "$final_path" == *".ssh/"* ]] || [[ "$final_path" == "/root/.ssh/"* ]]; then
            chmod 600 "$final_path"
        elif [[ "$final_path" == "/etc/ssh/ssh_host"* ]]; then
            chmod 600 "$final_path"
        elif [[ "$final_path" == "/etc/luks-keys/"* ]]; then
            chmod 600 "$final_path"
        elif [[ "$final_path" == "/root/.openclaw/"* ]]; then
            chmod 600 "$final_path"
        fi

        echo "Secret moved to: $final_path"
    else
        echo "Error: Required staged secret not found: $source_file" >&2
        exit 1
    fi
done

# Cleanup staging directory
rm -rf "$STAGE_DIR"

echo "=== Secrets applied ==="
SCRIPT_EOF

chmod +x /opt/cai/secrets/cai-secret-apply

# Enable cai-secret-apply service
systemctl enable cai-secret-apply.service

# Create dracut module for cai-secret-fetch
DRACUT_MODULE_DIR="/usr/lib/dracut/modules.d/99cai-secret-fetch"
mkdir -p "$DRACUT_MODULE_DIR"

# Create systemd service for initrd
cat > "$DRACUT_MODULE_DIR/cai-secret-fetch.service" << 'EOF'
[Unit]
Description=CAI Secret Fetch - Fetch secrets from Trustee
DefaultDependencies=no
ConditionPathExists=/etc/initrd-release
Requires=network-online.target
After=network-online.target
Before=initrd-root-device.target
Wants=attestation-agent.service
After=attestation-agent.service
Before=cryptpilot-fde-before-sysroot.service
# Critical service - failure should halt initrd boot process
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=oneshot
RemainAfterExit=true
ExecStart=/usr/bin/cai-secret-fetch
StandardOutput=journal+console
StandardError=journal+console
# Fail fast - if any secret fetch fails, the service should fail
Restart=no
SuccessExitStatus=0
# Critical failure handling - propagate failure to halt boot
OOMPolicy=kill-process-group
StartLimitIntervalSec=0

[Install]
RequiredBy=cryptpilot-fde-before-sysroot.service
# Also required by initrd target to ensure boot failure propagation
RequiredBy=initrd.target
EOF

# Create module-setup.sh
cat > "$DRACUT_MODULE_DIR/module-setup.sh" << 'EOF'
#!/bin/bash

check() { return 0; }

install() {
    inst_multiple confidential-data-hub curl date
    inst /opt/cai/secrets/cdh-config.toml
    inst /opt/cai/secrets/cai-secret-fetch /usr/bin/cai-secret-fetch
    inst_simple "$moddir/cai-secret-fetch.service" /usr/lib/systemd/system/cai-secret-fetch.service
    systemctl --root "$initdir" enable cai-secret-fetch.service
}

depends() {
    echo network
    echo confidential-data-hub
}
EOF

chmod +x "$DRACUT_MODULE_DIR/module-setup.sh"

echo "=== CAI secret management services configured ==="
