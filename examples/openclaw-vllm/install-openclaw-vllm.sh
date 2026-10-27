#!/bin/bash
set -euo pipefail
umask 022

MODEL_ID="${OPENCLAW_VLLM_MODEL_ID:-Qwen/Qwen3.6-35B-A3B}"
MODEL_DIR="${OPENCLAW_VLLM_MODEL_DIR:-/opt/models/Qwen3.6-35B-A3B}"
SERVED_MODEL_NAME="${OPENCLAW_VLLM_SERVED_MODEL_NAME:-Qwen3.6-35B-A3B}"
VLLM_PORT="${OPENCLAW_VLLM_PORT:-8090}"
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.28.0}"
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
NVIDIA_DRIVER_VERSION="${NVIDIA_DRIVER_VERSION:-580.82.07}"
NVIDIA_DRIVER_URL="${NVIDIA_DRIVER_URL:-https://cn.download.nvidia.cn/tesla/${NVIDIA_DRIVER_VERSION}/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run}"
NVIDIA_DRIVER_REFERER="${NVIDIA_DRIVER_REFERER:-https://www.nvidia.cn/}"
NVIDIA_DRIVER_SHA256="${NVIDIA_DRIVER_SHA256:-}"
CUDA_TOOLKIT_VERSION="${OPENCLAW_VLLM_CUDA_TOOLKIT_VERSION:-13.0.1}"
CUDA_BUNDLED_DRIVER_VERSION="${OPENCLAW_VLLM_CUDA_BUNDLED_DRIVER_VERSION:-580.82.07}"
CUDA_TOOLKIT_URL="${OPENCLAW_VLLM_CUDA_TOOLKIT_URL:-https://developer.download.nvidia.cn/compute/cuda/${CUDA_TOOLKIT_VERSION}/local_installers/cuda_${CUDA_TOOLKIT_VERSION}_${CUDA_BUNDLED_DRIVER_VERSION}_linux.run}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
BUILD_POSTINSTALL_MARKER=/var/lib/cai/openclaw-vllm/build-postinstall.done

ensure_openclaw_runtime_ownership() {
  if ! id -u openclaw >/dev/null 2>&1; then
    return 0
  fi
  install -d -m 0750 -o openclaw -g openclaw /home/openclaw /home/openclaw/.openclaw
  install -d -m 0755 -o openclaw -g openclaw /home/openclaw/.openclaw/skills
  install -d -m 0775 -o openclaw -g openclaw /workspace
  chown -R openclaw:openclaw /home/openclaw /workspace
  chmod 0750 /home/openclaw/.openclaw
  chmod 0775 /workspace
}

if [[ -f "$BUILD_POSTINSTALL_MARKER" ]]; then
  ensure_openclaw_runtime_ownership
  echo "OpenClaw vLLM build postinstall already completed; fixed runtime ownership and skipped repeated mkosi postinstall pass"
  exit 0
fi

ensure_build_dependencies() {
  local missing=()
  for cmd in curl gcc c++ git jq make npm python3.11 rpm sha256sum wget; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  command -v depmod >/dev/null 2>&1 || missing+=("kmod")
  command -v modprobe >/dev/null 2>&1 || missing+=("kmod")
  command -v lspci >/dev/null 2>&1 || missing+=("pciutils")
  python3.11 -m pip --version >/dev/null 2>&1 || missing+=("python3.11-pip")
  if [[ ! -d /usr/src/kernels ]] || ! find /usr/src/kernels -mindepth 1 -maxdepth 1 -type d | grep -q .; then
    missing+=("kernel-devel")
  fi
  if ((${#missing[@]} > 0)); then
    printf 'missing OpenClaw vLLM build dependencies: %s\n' "${missing[*]}" >&2
    printf 'add the corresponding packages to build.packages in the Confidential Agent spec\n' >&2
    exit 1
  fi
}

target_kernel_version() {
  find /lib/modules -mindepth 1 -maxdepth 1 -type d -printf '%f\n' 2>/dev/null | sort -V | tail -n 1
}

verify_nvidia_driver_checksum() {
  local runfile="$1"
  if [[ -z "$NVIDIA_DRIVER_SHA256" ]]; then
    return 0
  fi
  local actual
  actual="$(sha256sum "$runfile" | awk '{print $1}')"
  if [[ "$actual" != "$NVIDIA_DRIVER_SHA256" ]]; then
    echo "NVIDIA driver checksum mismatch: expected $NVIDIA_DRIVER_SHA256 got $actual" >&2
    return 1
  fi
}

download_installer() {
  local url="$1" destination="$2" referer="${3:-}"
  local extra=()
  [[ -z "$referer" ]] || extra+=(--referer "$referer")
  # curl 7.61 does not retry every transport failure (including HTTP/2 error
  # 92). Keep one bounded retry loop, and avoid HTTP/2 for these large assets.
  local attempt status http_status
  for attempt in 1 2 3 4; do
    rm -f "$destination.tmp"
    if http_status="$(curl --http1.1 --fail --location \
        --connect-timeout 20 --max-time 1800 --speed-time 60 --speed-limit 1024 \
        "${extra[@]}" --write-out '%{http_code}' --output "$destination.tmp" --url "$url")"; then
      if mv "$destination.tmp" "$destination"; then
        return 0
      else
        status=$?
        rm -f "$destination.tmp"
        return "$status"
      fi
    else
      status=$?
    fi
    rm -f "$destination.tmp"
    case "$status" in
      5|6|7|16|18|28|35|52|55|56|92) ;;
      22)
        case "$http_status" in
          408|429|500|502|503|504) ;;
          *) return "$status" ;;
        esac
        ;;
      *) return "$status" ;;
    esac
    if ((attempt == 4)); then
      return "$status"
    fi
    printf 'installer download failed (curl=%s http=%s attempt=%s/4); retrying\n' \
      "$status" "$http_status" "$attempt" >&2
    sleep 2
  done
}

download_nvidia_driver() {
  local state_dir runfile
  state_dir=/var/lib/cai/nvidia-cc
  runfile="$state_dir/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run"
  mkdir -p "$state_dir"
  if [[ -s "$runfile" ]]; then
    verify_nvidia_driver_checksum "$runfile"
    chmod 0755 "$runfile"
    printf '%s\n' "$runfile"
    return
  fi
  echo "downloading NVIDIA driver $NVIDIA_DRIVER_VERSION" >&2
  download_installer "$NVIDIA_DRIVER_URL" "$runfile" "$NVIDIA_DRIVER_REFERER" || return 1
  verify_nvidia_driver_checksum "$runfile" || { rm -f "$runfile"; return 1; }
  chmod 0755 "$runfile"
  printf '%s\n' "$runfile"
}

preinstall_nvidia_driver() {
  local kernel runfile state_dir
  state_dir=/var/lib/cai/nvidia-cc
  kernel="$(target_kernel_version)"
  if [[ -z "$kernel" ]]; then
    echo "unable to determine target kernel from /lib/modules" >&2
    return 1
  fi
  echo "pre-installing NVIDIA driver $NVIDIA_DRIVER_VERSION for target kernel $kernel"
  if find "/lib/modules/$kernel" -name 'nvidia*.ko*' -print -quit 2>/dev/null | grep -q . &&
     command -v nvidia-smi >/dev/null 2>&1 &&
     command -v nvidia-persistenced >/dev/null 2>&1; then
    touch "$state_dir/build-preinstalled.done"
    return 0
  fi
  if [[ ! -e "/lib/modules/$kernel/build" && -d "/usr/src/kernels/$kernel" ]]; then
    ln -sfn "/usr/src/kernels/$kernel" "/lib/modules/$kernel/build"
  fi
  if [[ ! -e "/lib/modules/$kernel/build" ]]; then
    echo "missing kernel build tree for target kernel $kernel" >&2
    return 1
  fi
  runfile="$(download_nvidia_driver)"
  bash "$runfile" \
    --ui=none \
    --no-questions \
    --accept-license \
    --disable-nouveau \
    --no-cc-version-check \
    --install-libglvnd \
    --kernel-module-build-directory=kernel-open \
    --kernel-name="$kernel" || true
  depmod -a "$kernel" 2>/dev/null || true
  if ! find "/lib/modules/$kernel" -name 'nvidia*.ko*' -print -quit 2>/dev/null | grep -q .; then
    echo "NVIDIA kernel modules were not installed for target kernel $kernel" >&2
    return 1
  fi
  touch "$state_dir/build-preinstalled.done"
  rm -f "$runfile"
}

preinstall_cuda_toolkit() {
  local state_dir runfile
  if [[ -x /usr/local/cuda/bin/nvcc ]]; then
    return 0
  fi
  state_dir=/var/lib/cai/nvidia-cc
  runfile="$state_dir/cuda_${CUDA_TOOLKIT_VERSION}_${CUDA_BUNDLED_DRIVER_VERSION}_linux.run"
  mkdir -p "$state_dir"
  if [[ ! -s "$runfile" ]]; then
    echo "downloading CUDA toolkit $CUDA_TOOLKIT_VERSION"
    download_installer "$CUDA_TOOLKIT_URL" "$runfile" || return 1
    chmod 0755 "$runfile"
  fi
  bash "$runfile" --silent --toolkit
  /usr/local/cuda/bin/nvcc --version
  rm -f "$runfile"
}

groupadd -r openclaw 2>/dev/null || true
id -u openclaw >/dev/null 2>&1 || useradd -r -g openclaw -d /home/openclaw -m -s /bin/bash openclaw
install -d -m 0755 /usr/local/share/cai /var/lib/cai /workspace
install -d -m 0750 -o openclaw -g openclaw /home/openclaw/.openclaw /home/openclaw/.openclaw/skills
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh setup-runtime openclaw /home/openclaw/.openclaw
chown -R openclaw:openclaw /home/openclaw /workspace
chmod 0775 /workspace

ensure_build_dependencies
install -d -m 0755 /etc/modprobe.d
cat >/etc/modprobe.d/blacklist-nouveau.conf <<'EOF'
blacklist nouveau
options nouveau modeset=0
EOF

preserve_runtime_build_inputs() {
  local kernel build_root kernel_src
  kernel="$(target_kernel_version)"
  [[ -n "$kernel" ]] || return 0
  build_root=/opt/confidential-agent/openclaw-vllm
  kernel_src="/usr/src/kernels/$kernel"
  if [[ ! -d "$kernel_src" ]]; then
    kernel_src="$(find /usr/src/kernels -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | head -n 1 || true)"
  fi
  if [[ -d "$kernel_src" ]]; then
    install -d -m 0755 "$build_root/kernel-build"
    rm -rf "$build_root/kernel-build/$(basename "$kernel_src")"
    cp -a "$kernel_src" "$build_root/kernel-build/"
  fi
  if [[ -d /usr/include ]]; then
    rm -rf "$build_root/usr-include"
    install -d -m 0755 "$build_root"
    cp -a /usr/include "$build_root/usr-include"
  fi
}

preserve_runtime_build_inputs
if ! preinstall_nvidia_driver; then
  if [[ "${CAI_NVIDIA_DRIVER_PREINSTALL_REQUIRED:-1}" == "1" ]]; then
    echo "failed to pre-install NVIDIA driver during image build" >&2
    exit 1
  fi
  echo "warning: failed to pre-install NVIDIA driver; guest bootstrap will only try loading existing modules" >&2
fi
if [[ "${OPENCLAW_VLLM_PREINSTALL_CUDA_TOOLKIT:-1}" == "1" ]] && ! preinstall_cuda_toolkit; then
  if [[ "${CAI_CUDA_TOOLKIT_PREINSTALL_REQUIRED:-1}" == "1" ]]; then
    echo "failed to pre-install CUDA toolkit during image build" >&2
    exit 1
  fi
  echo "warning: failed to pre-install CUDA toolkit; relying on Python wheel runtime libraries" >&2
fi

install -D -m 0644 /usr/local/share/cai/nvidia-persistenced.service /usr/lib/systemd/system/nvidia-persistenced.service
chmod 0755 /usr/local/sbin/cai-nvidia-cc-stack-install.sh

cat >/etc/systemd/system/cai-nvidia-cc-bootstrap.service <<'EOF'
[Unit]
Description=CAI NVIDIA CC GPU driver bootstrap
After=local-fs.target network-online.target
Wants=network-online.target
Before=cai-vllm.service
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=7200
ExecStart=/usr/local/sbin/cai-nvidia-cc-stack-install.sh

[Install]
WantedBy=multi-user.target
EOF

cat >/usr/local/bin/cai-vllm-install-deps.sh <<'EOF'
#!/bin/bash
set -euo pipefail
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.28.0}"
export PIP_CACHE_DIR="${PIP_CACHE_DIR:-/tmp/cai-pip-cache}"
export UV_CACHE_DIR="${UV_CACHE_DIR:-/tmp/cai-uv-cache}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-/tmp/cai-cache}"
cleanup_package_caches() {
  rm -rf "$PIP_CACHE_DIR" "$UV_CACHE_DIR" "$XDG_CACHE_HOME" /root/.cache/pip /root/.cache/uv
}
trap cleanup_package_caches EXIT
restore_vllm_build_headers() {
  local src=/opt/confidential-agent/openclaw-vllm/usr-include
  if [[ -f /usr/include/python3.11/Python.h && -f /usr/include/stdio.h ]]; then
    return 0
  fi
  if [[ ! -d "$src" ]]; then
    echo "missing preserved build headers at $src" >&2
    return 1
  fi
  mkdir -p /usr/include
  cp -a "$src"/. /usr/include/
}
vllm_deps_ready() {
  [[ -x /root/.venv/bin/python && -x /root/.venv/bin/vllm && -x /root/.venv/bin/ninja ]] || return 1
  VLLM_VERSION="$VLLM_VERSION" /root/.venv/bin/python - <<'PY' || return 1
import importlib.metadata
import os
raise SystemExit(0 if importlib.metadata.version("vllm") == os.environ["VLLM_VERSION"] else 1)
PY
  python3.11 - <<'PY' || return 1
import importlib
importlib.import_module("modelscope")
PY
}
restore_vllm_build_headers
if vllm_deps_ready; then
  cleanup_package_caches
  exit 0
fi
python3.11 -m pip --version >/dev/null
python3.11 -m pip install -i "$PYPI_INDEX_URL" uv 'modelscope>=1.15.0' importlib-metadata
mkdir -p /etc/uv
cat >/etc/uv/uv.toml <<UV
[[index]]
url = "$PYPI_INDEX_URL"
default = true
UV
install -d -m 0700 /root
cd /root
if [[ ! -d .venv ]]; then
  uv venv --python 3.11
fi
uv pip install -i "$PYPI_INDEX_URL" --only-binary=:all: "vllm==$VLLM_VERSION" ninja
cleanup_package_caches
EOF
chmod 0755 /usr/local/bin/cai-vllm-install-deps.sh
if [[ "${OPENCLAW_VLLM_PREINSTALL_VLLM:-1}" == "1" ]]; then
  /usr/local/bin/cai-vllm-install-deps.sh
else
  PIP_CACHE_DIR=/tmp/cai-pip-cache python3.11 -m pip install -i "$PYPI_INDEX_URL" uv 'modelscope>=1.15.0' importlib-metadata
  rm -rf /tmp/cai-pip-cache /root/.cache/pip
fi

cat >/usr/local/bin/cai-vllm-setup.sh <<'EOF'
#!/bin/bash
set -euo pipefail
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.28.0}"
restore_vllm_build_headers() {
  local src=/opt/confidential-agent/openclaw-vllm/usr-include
  if [[ -f /usr/include/python3.11/Python.h && -f /usr/include/stdio.h ]]; then
    return 0
  fi
  if [[ ! -d "$src" ]]; then
    echo "missing preserved build headers at $src" >&2
    return 1
  fi
  mkdir -p /usr/include
  cp -a "$src"/. /usr/include/
}
restore_vllm_build_headers
cd /root
command -v uv >/dev/null 2>&1 || { echo "uv is missing; image build must preinstall vLLM dependencies" >&2; exit 1; }
[[ -x /root/.venv/bin/python && -x /root/.venv/bin/vllm ]] || { echo "vLLM virtualenv is missing; image build must preinstall vLLM dependencies" >&2; exit 1; }
if ! VLLM_VERSION="$VLLM_VERSION" /root/.venv/bin/python - <<'PY'
import importlib.metadata
import os
raise SystemExit(0 if importlib.metadata.version("vllm") == os.environ["VLLM_VERSION"] else 1)
PY
then
  echo "vLLM version mismatch; image build must preinstall the requested version" >&2
  exit 1
fi
if ! python3.11 - <<'PY'
import importlib
importlib.import_module("modelscope")
PY
then
  echo "modelscope is missing; image build must preinstall model fetch dependencies" >&2
  exit 1
fi
test -x /root/.venv/bin/python
test -x /root/.venv/bin/vllm
EOF
chmod 0755 /usr/local/bin/cai-vllm-setup.sh

cat >/usr/local/bin/cai-modelscope-fetch.sh <<EOF
#!/bin/bash
set -euo pipefail
MODEL_ID="$MODEL_ID"
MODEL_DIR="$MODEL_DIR"
PYPI_INDEX_URL="$PYPI_INDEX_URL"
if [[ -f "\$MODEL_DIR/config.json" ]] || [[ -f "\$MODEL_DIR/configuration.json" ]]; then
  exit 0
fi
if ! /usr/bin/python3.11 - <<'PY_CHECK'
import importlib
importlib.import_module("modelscope")
PY_CHECK
then
  echo "modelscope is missing; image build must preinstall model fetch dependencies" >&2
  exit 1
fi
mkdir -p "\$MODEL_DIR"
export MODELSCOPE_CACHE="\${MODELSCOPE_CACHE:-/opt/modelscope-cache}"
mkdir -p "\$MODELSCOPE_CACHE"
for attempt in 1 2 3 4 5; do
  if /usr/bin/python3.11 - <<PY
from modelscope.hub.snapshot_download import snapshot_download
snapshot_download("${MODEL_ID}", cache_dir="${MODELSCOPE_CACHE:-/opt/modelscope-cache}", local_dir="${MODEL_DIR}")
PY
  then
    exit 0
  fi
  echo "model download attempt \${attempt} failed, retrying" >&2
  sleep 60
done
exit 1
EOF
chmod 0755 /usr/local/bin/cai-modelscope-fetch.sh
if [[ "${OPENCLAW_VLLM_PRELOAD_MODEL:-0}" == "1" ]]; then
  /usr/local/bin/cai-modelscope-fetch.sh
fi

cat >/usr/local/bin/cai-vllm-wait-deps.sh <<EOF
#!/bin/bash
set -euo pipefail
MODEL_DIR="$MODEL_DIR"

for svc in cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-modelscope-fetch.service; do
  systemctl reset-failed "\$svc" 2>/dev/null || true
  systemctl start --no-block "\$svc"
done

for _ in \$(seq 1 1440); do
  if systemctl is-active --quiet cai-nvidia-cc-bootstrap.service &&
     systemctl is-active --quiet nvidia-persistenced.service &&
     systemctl is-active --quiet cai-modelscope-fetch.service &&
     [[ -e /dev/nvidia0 && ( -f "\$MODEL_DIR/config.json" || -f "\$MODEL_DIR/configuration.json" ) ]]; then
    exit 0
  fi
  sleep 5
done

systemctl status cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-modelscope-fetch.service --no-pager -l || true
ls -la "\$MODEL_DIR" 2>/dev/null || true
exit 1
EOF
chmod 0755 /usr/local/bin/cai-vllm-wait-deps.sh

cat >/usr/local/bin/cai-vllm-run.sh <<EOF
#!/bin/bash
set -euo pipefail
if [[ ! -f /usr/include/python3.11/Python.h && -d /opt/confidential-agent/openclaw-vllm/usr-include ]]; then
  mkdir -p /usr/include
  cp -a /opt/confidential-agent/openclaw-vllm/usr-include/. /usr/include/
fi
cd /root
/root/.venv/bin/vllm serve "$MODEL_DIR/" \\
  --enable-auto-tool-choice --tool-call-parser qwen3_coder \\
  --port "$VLLM_PORT" --host 127.0.0.1 --served-model-name "$SERVED_MODEL_NAME" \\
  --max-num-seqs 512 \\
  --gdn-prefill-backend triton
EOF
chmod 0755 /usr/local/bin/cai-vllm-run.sh

/usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh openclaw /home/openclaw

cat >/usr/local/bin/cai-openclaw-vllm-runtime-check.sh <<'EOF'
#!/bin/bash
set -euo pipefail

command -v node >/dev/null
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)'
command -v openclaw >/dev/null
test -d "$(npm root -g)/openclaw/dist"
test -d /home/openclaw/.openclaw/extensions/dingtalk
test -f /home/openclaw/.openclaw/extensions/dingtalk/dist/index.js
test -d /home/openclaw/.openclaw/extensions/cai-pep
test -d /home/openclaw/.openclaw/extensions/cai-a2a
EOF
chmod 0755 /usr/local/bin/cai-openclaw-vllm-runtime-check.sh

cat >/etc/systemd/system/cai-openclaw-vllm-runtime-bootstrap.service <<'EOF'
[Unit]
Description=CAI verify OpenClaw runtime dependencies
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=120
ExecStart=/usr/local/bin/cai-openclaw-vllm-runtime-check.sh
StandardOutput=journal
StandardError=journal
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/etc/systemd/system/cai-modelscope-fetch.service <<'EOF'
[Unit]
Description=CAI download OpenClaw vLLM model from ModelScope
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=7200
Restart=on-failure
RestartSec=60
ExecStart=/usr/local/bin/cai-modelscope-fetch.sh
StandardOutput=journal
StandardError=journal
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/etc/systemd/system/cai-vllm.service <<'EOF'
[Unit]
Description=CAI vLLM OpenAI server
After=network-online.target cai-nvidia-cc-bootstrap.service cai-modelscope-fetch.service nvidia-persistenced.service
Wants=network-online.target cai-nvidia-cc-bootstrap.service cai-modelscope-fetch.service nvidia-persistenced.service
ConditionPathExists=/dev/nvidia0
StartLimitIntervalSec=0

[Service]
Type=simple
Restart=on-failure
RestartSec=20
TimeoutStartSec=10800
ExecStartPre=/usr/local/bin/cai-vllm-wait-deps.sh
ExecStartPre=/usr/local/bin/cai-vllm-setup.sh
ExecStart=/usr/local/bin/cai-vllm-run.sh
StandardOutput=journal
StandardError=journal
Environment=HOME=/root
Environment=PATH=/root/.venv/bin:/usr/local/cuda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/usr/local/bin/cai-openclaw-vllm-bootstrap <<EOF
#!/bin/bash
set -euo pipefail
for _ in \$(seq 1 120); do
  if [[ -e /dev/nvidia0 ]] && curl -sf "http://127.0.0.1:$VLLM_PORT/v1/models" >/dev/null; then
    exit 0
  fi
  sleep 3
done
echo "GPU and vLLM did not become ready on port $VLLM_PORT" >&2
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-vllm-bootstrap

cat >/usr/local/bin/cai-openclaw-gateway-wait-deps.sh <<EOF
#!/bin/bash
set -euo pipefail

for _ in \$(seq 1 1440); do
  if systemctl is-active --quiet cai-openclaw-vllm-runtime-bootstrap.service &&
     systemctl is-active --quiet cai-vllm.service &&
     curl -fsS --max-time 5 "http://127.0.0.1:$VLLM_PORT/v1/models" >/dev/null; then
    exit 0
  fi
  sleep 5
done

systemctl status cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service --no-pager -l || true
curl -fsS --max-time 5 "http://127.0.0.1:$VLLM_PORT/v1/models" || true
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-gateway-wait-deps.sh

cat >/etc/systemd/system/cai-openclaw-gateway.service <<'EOF'
[Unit]
Description=CAI OpenClaw Gateway
After=network-online.target cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service
Wants=network-online.target cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service
StartLimitIntervalSec=0

[Service]
Type=simple
User=openclaw
Group=openclaw
WorkingDirectory=/home/openclaw
Environment=HOME=/home/openclaw
Environment=TMPDIR=/tmp
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
Environment=OPENCLAW_CONFIG_PATH=/home/openclaw/.openclaw/openclaw.json
Environment=OPENCLAW_NO_RESPAWN=1
Environment=OPENCLAW_DISABLE_BONJOUR=1
TimeoutStartSec=10800
ExecStartPre=/usr/local/bin/cai-openclaw-gateway-wait-deps.sh
ExecStartPre=/usr/local/bin/cai-openclaw-vllm-bootstrap
ExecStart=/usr/local/bin/openclaw gateway run --port 18789 --bind lan
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-nvidia-cc-bootstrap.service
systemctl enable nvidia-persistenced.service || true
systemctl enable cai-openclaw-vllm-runtime-bootstrap.service
systemctl enable cai-modelscope-fetch.service
systemctl enable cai-vllm.service
systemctl enable cai-openclaw-gateway.service

install -d -m 0755 "$(dirname "$BUILD_POSTINSTALL_MARKER")"
touch "$BUILD_POSTINSTALL_MARKER"
ensure_openclaw_runtime_ownership
