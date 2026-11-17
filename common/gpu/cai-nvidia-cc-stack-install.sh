#!/bin/bash
set -euo pipefail

STATE_DIR=/var/lib/cai/nvidia-cc
LOG_TAG=cai-nvidia-cc
mkdir -p "$STATE_DIR"
exec >>/var/log/cai-nvidia-cc-install.log 2>&1

log() {
  echo "[$(date -Iseconds)] $LOG_TAG $*"
}

have_nvidia_pci() {
  command -v lspci >/dev/null 2>&1 || return 1
  lspci -mm 2>/dev/null | grep -qi nvidia && return 0
  lspci 2>/dev/null | grep -qiE 'nvidia|3D controller' && return 0
  return 1
}

write_modprobe_config() {
  mkdir -p /etc/modprobe.d
  cat >/etc/modprobe.d/blacklist-nouveau.conf <<'CONF'
blacklist nouveau
options nouveau modeset=0
CONF
  cat >/etc/modprobe.d/nvidia-confidential-compute.conf <<'CONF'
softdep nvidia pre: ecdh_generic ecdsa_generic
CONF
}

verify_nouveau_absent() {
  if lsmod | grep -q '^nouveau'; then
    log "ERROR: nouveau is loaded before NVIDIA driver initialization."
    log "ERROR: add rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0 to kernel_cmdline_append."
    exit 1
  fi
}

load_driver() {
  depmod -a 2>/dev/null || true
  modprobe ecdsa_generic 2>/dev/null || true
  modprobe ecdh_generic 2>/dev/null || modprobe ecdh 2>/dev/null || true
  modprobe nvidia 2>/dev/null || true
  modprobe nvidia-uvm 2>/dev/null || true
  command -v nvidia-modprobe >/dev/null 2>&1 && nvidia-modprobe -u -c=0 || true
}

wait_for_device_node() {
  for _ in $(seq 1 30); do
    [[ -e /dev/nvidia0 ]] && return 0
    sleep 1
  done
  return 1
}

wait_persistenced_active() {
  local status
  for _ in $(seq 1 300); do
    if systemctl is-active --quiet nvidia-persistenced.service 2>/dev/null; then
      status="$(systemctl status nvidia-persistenced.service 2>/dev/null | grep 'Active: ' || true)"
      log "nvidia-persistenced: $status"
      if echo "$status" | grep -q 'active (running)'; then
        return 0
      fi
    fi
    sleep 2
  done
  log "WARN: nvidia-persistenced did not reach active (running) in time."
  return 1
}

start_services() {
  install -m 0644 /usr/local/share/cai/nvidia-persistenced.service \
    /usr/lib/systemd/system/nvidia-persistenced.service 2>/dev/null || true
  systemctl daemon-reload 2>/dev/null || true
  systemctl enable nvidia-persistenced.service 2>/dev/null || true
  systemctl reset-failed nvidia-persistenced.service 2>/dev/null || true
  systemctl start nvidia-persistenced.service 2>/dev/null || true
  wait_persistenced_active || true
  systemctl enable cloudmonitor.service 2>/dev/null || true
  systemctl start cloudmonitor.service 2>/dev/null || true
}

if ! have_nvidia_pci; then
  log "No NVIDIA PCI device detected; skip CC GPU stack."
  exit 0
fi

write_modprobe_config
verify_nouveau_absent

if [[ -e /dev/nvidia0 ]]; then
  log "Kernel driver present (/dev/nvidia0)."
  start_services
  exit 0
fi

if [[ ! -f "$STATE_DIR/build-preinstalled.done" ]]; then
  log "WARN: NVIDIA driver was not marked preinstalled during image build; trying prebuilt modules anyway."
fi

log "Loading prebuilt NVIDIA kernel modules..."
load_driver

if wait_for_device_node; then
  log "Modules loaded, /dev/nvidia0 present."
  start_services
  exit 0
fi

if [[ ! -f "$STATE_DIR/post-install-reboot.done" ]]; then
  log "WARN: Modules loaded but /dev/nvidia0 absent; scheduling one reboot."
  touch "$STATE_DIR/post-install-reboot.done"
  systemctl reboot --no-block
  exit 0
fi

log "ERROR: /dev/nvidia0 absent after reboot."
systemctl status nvidia-persistenced.service --no-pager -l || true
dmesg | tail -200 || true
exit 1
