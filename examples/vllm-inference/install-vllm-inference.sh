#!/bin/bash
set -euo pipefail

VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.19.1}"
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
NVIDIA_DRIVER_VERSION="${NVIDIA_DRIVER_VERSION:-550.144.03}"
NVIDIA_DRIVER_URL="${NVIDIA_DRIVER_URL:-https://cn.download.nvidia.cn/tesla/${NVIDIA_DRIVER_VERSION}/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run}"
NVIDIA_DRIVER_REFERER="${NVIDIA_DRIVER_REFERER:-https://www.nvidia.cn/}"
NVIDIA_DRIVER_SHA256="${NVIDIA_DRIVER_SHA256:-}"
CUDA_TOOLKIT_VERSION="${OPENCLAW_VLLM_CUDA_TOOLKIT_VERSION:-12.4.1}"
CUDA_BUNDLED_DRIVER_VERSION="${OPENCLAW_VLLM_CUDA_BUNDLED_DRIVER_VERSION:-550.54.15}"
CUDA_TOOLKIT_URL="${OPENCLAW_VLLM_CUDA_TOOLKIT_URL:-https://developer.download.nvidia.com/compute/cuda/${CUDA_TOOLKIT_VERSION}/local_installers/cuda_${CUDA_TOOLKIT_VERSION}_${CUDA_BUNDLED_DRIVER_VERSION}_linux.run}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
BUILD_POSTINSTALL_MARKER=/var/lib/cai/vllm-inference/build-postinstall.done

if [[ -f "$BUILD_POSTINSTALL_MARKER" ]]; then
  echo "vLLM inference build postinstall already completed; skipping repeated mkosi postinstall pass"
  exit 0
fi

# Debug image variant only: install openssh-server so the guest daemon can
# enable+start sshd when the debug-ssh marker is present. Release images stay
# sshd-free; this block is a no-op without the marker.
if [[ -f /etc/confidential-agent/debug-ssh-enabled ]]; then
  dnf install -y openssh-server || yum install -y openssh-server || true
fi

ensure_build_dependencies() {
  local missing=()
  for cmd in curl gcc git jq make python3.11 rpm sha256sum wget; do
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
    printf 'missing vLLM inference build dependencies: %s\n' "${missing[*]}" >&2
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
  if ! wget --referer="$NVIDIA_DRIVER_REFERER" -O "$runfile.tmp" "$NVIDIA_DRIVER_URL"; then
    rm -f "$runfile.tmp"
    return 1
  fi
  mv "$runfile.tmp" "$runfile"
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
  if [[ -x /usr/local/cuda/bin/nvcc || -d /usr/local/cuda/targets/x86_64-linux/lib ]]; then
    return 0
  fi
  state_dir=/var/lib/cai/nvidia-cc
  runfile="$state_dir/cuda_${CUDA_TOOLKIT_VERSION}_${CUDA_BUNDLED_DRIVER_VERSION}_linux.run"
  mkdir -p "$state_dir"
  if [[ ! -s "$runfile" ]]; then
    echo "downloading CUDA toolkit $CUDA_TOOLKIT_VERSION"
    if ! wget -O "$runfile.tmp" "$CUDA_TOOLKIT_URL"; then
      rm -f "$runfile.tmp"
      return 1
    fi
    mv "$runfile.tmp" "$runfile"
    chmod 0755 "$runfile"
  fi
  bash "$runfile" --silent --toolkit
  rm -f "$runfile"
}

install -d -m 0755 /usr/local/share/cai /var/lib/cai

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
if [[ "${OPENCLAW_VLLM_PREINSTALL_CUDA_TOOLKIT:-0}" == "1" ]] && ! preinstall_cuda_toolkit; then
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
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.19.1}"
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
  [[ -x /root/.venv/bin/python && -x /root/.venv/bin/vllm ]] || return 1
  VLLM_VERSION="$VLLM_VERSION" /root/.venv/bin/python - <<'PY' || return 1
import importlib.metadata
import os
raise SystemExit(0 if importlib.metadata.version("vllm") == os.environ["VLLM_VERSION"] else 1)
PY
}
restore_vllm_build_headers
if vllm_deps_ready; then
  cleanup_package_caches
  exit 0
fi
python3.11 -m pip --version >/dev/null
python3.11 -m pip install -i "$PYPI_INDEX_URL" uv importlib-metadata
mkdir -p /etc/uv
cat >/etc/uv/uv.toml <<UV
[[index]]
url = "$PYPI_INDEX_URL"
default = true
UV
cd /root
if [[ ! -d .venv ]]; then
  uv venv --python 3.11
fi
uv pip install -i "$PYPI_INDEX_URL" --only-binary=:all: "vllm==$VLLM_VERSION"
cleanup_package_caches
EOF
chmod 0755 /usr/local/bin/cai-vllm-install-deps.sh
if [[ "${OPENCLAW_VLLM_PREINSTALL_VLLM:-1}" == "1" ]]; then
  /usr/local/bin/cai-vllm-install-deps.sh
else
  PIP_CACHE_DIR=/tmp/cai-pip-cache python3.11 -m pip install -i "$PYPI_INDEX_URL" uv importlib-metadata
  rm -rf /tmp/cai-pip-cache /root/.cache/pip
fi

cat >/usr/local/bin/cai-vllm-setup.sh <<'EOF'
#!/bin/bash
set -euo pipefail
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.19.1}"
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
test -x /root/.venv/bin/python
test -x /root/.venv/bin/vllm
EOF
chmod 0755 /usr/local/bin/cai-vllm-setup.sh

# --- native s3fs + gocryptfs decryption stack ---
# gocryptfs is in alinux3-plus (already in build.packages); s3fs-fuse is in EPEL.
# Enable the Aliyun internal EPEL mirror (reachable from the build host's VPC).
install -d -m 0755 /etc/yum.repos.d
cat >/etc/yum.repos.d/epel-cai.repo <<'EOF'
[epel-cai]
name=Aliyun EPEL mirror for s3fs-fuse
baseurl=http://mirrors.cloud.aliyuncs.com/epel/8/Everything/$basearch/
enabled=1
gpgcheck=0
EOF
yum install -y gocryptfs fuse s3fs-fuse
install -d -m 0755 /etc/cai

cat >/usr/local/bin/cai-s3fs-start.sh <<'UNIT'
#!/usr/bin/env bash
set -euo pipefail
source /etc/cai/model.env
install -d -m 0755 /tmp/oss /tmp/cachefs-key
# s3fs 1.97 (libfuse3) hangs on subpath mounts: CheckBucket HEADs the
# subpath prefix, OSS returns 404, and the threadpool retries forever.
# Mount the bucket root and let gocryptfs reference the subpath inside.
MODEL_OSS_BUCKET_NAME="${MODEL_OSS_BUCKET%%:/*}"
exec s3fs "${MODEL_OSS_BUCKET_NAME}" /tmp/oss \
  -o "url=${MODEL_OSS_ENDPOINT}" \
  -o public_bucket=1 -o allow_other -o auto_unmount \
  -o entry_timeout=100000 -o attr_timeout=10000 \
  -o multipart_size=24 -o parallel_count=48 -f
UNIT
chmod 0755 /usr/local/bin/cai-s3fs-start.sh

cat >/usr/local/bin/cai-gocryptfs-start.sh <<'UNIT'
#!/usr/bin/env bash
set -euo pipefail
source /etc/cai/model.env
KEY_FILE=/run/cai/secrets/model-decryption-key
# wait for the challenge-injected key
for _ in $(seq 1 120); do
  [[ -s "$KEY_FILE" ]] && break
  sleep 5
done
[[ -s "$KEY_FILE" ]] || { echo "model decryption key not injected" >&2; exit 1; }
install -d -m 0755 "$MODEL_MOUNT_POINT"
# s3fs mounts the bucket root; extract the subpath (format: bucket:/subpath)
MODEL_OSS_SUBPATH="${MODEL_OSS_BUCKET#*:}"
MODEL_OSS_SUBPATH="${MODEL_OSS_SUBPATH#/}"
CIPHER_DIR="/tmp/oss/${MODEL_OSS_SUBPATH}"
# wait for the s3fs ciphertext dir to expose a gocryptfs volume
for _ in $(seq 1 120); do
  [[ -f "${CIPHER_DIR}/gocryptfs.conf" ]] && break
  sleep 5
done
exec gocryptfs -f -passfile "$KEY_FILE" "$CIPHER_DIR" "$MODEL_MOUNT_POINT"
UNIT
chmod 0755 /usr/local/bin/cai-gocryptfs-start.sh

cat >/etc/systemd/system/cai-s3fs.service <<'UNIT'
[Unit]
Description=CAI s3fs OSS ciphertext mount
After=network-online.target cai-nvidia-cc-bootstrap.service
Before=cai-gocryptfs.service cai-vllm.service

[Service]
Type=simple
EnvironmentFile=/etc/cai/model.env
ExecStart=/usr/local/bin/cai-s3fs-start.sh
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

cat >/etc/systemd/system/cai-gocryptfs.service <<'UNIT'
[Unit]
Description=CAI gocryptfs model decryption mount
After=cai-s3fs.service
Before=cai-vllm.service

[Service]
Type=simple
EnvironmentFile=/etc/cai/model.env
ExecStart=/usr/local/bin/cai-gocryptfs-start.sh
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

cat >/usr/local/bin/cai-vllm-wait-deps.sh <<'UNIT'
#!/usr/bin/env bash
set -euo pipefail
source /etc/cai/model.env
# wait for GPU + decrypted model
for _ in $(seq 1 1440); do
  [[ -e /dev/nvidia0 && -f "$MODEL_MOUNT_POINT/config.json" ]] && exit 0
  sleep 5
done
echo "vLLM deps not ready: /dev/nvidia0 or $MODEL_MOUNT_POINT/config.json missing" >&2
exit 1
UNIT
chmod 0755 /usr/local/bin/cai-vllm-wait-deps.sh

cat >/usr/local/bin/cai-vllm-run.sh <<'UNIT'
#!/usr/bin/env bash
set -euo pipefail
source /etc/cai/model.env
exec /root/.venv/bin/vllm serve "$MODEL_MOUNT_POINT" \
  --host 0.0.0.0 --port "$VLLM_PORT" \
  --served-model-name "$VLLM_SERVED_MODEL_NAME"
UNIT
chmod 0755 /usr/local/bin/cai-vllm-run.sh

cat >/etc/systemd/system/cai-vllm.service <<'UNIT'
[Unit]
Description=CAI vLLM inference server
After=cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-s3fs.service cai-gocryptfs.service
Wants=cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-s3fs.service cai-gocryptfs.service
ConditionPathExists=/dev/nvidia0

[Service]
Type=simple
EnvironmentFile=/etc/cai/model.env
ExecStartPre=/usr/local/bin/cai-vllm-wait-deps.sh
ExecStartPre=/usr/local/bin/cai-vllm-setup.sh
ExecStart=/usr/local/bin/cai-vllm-run.sh
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload || true
systemctl enable cai-nvidia-cc-bootstrap.service nvidia-persistenced.service \
  cai-s3fs.service cai-gocryptfs.service cai-vllm.service

install -d -m 0755 "$(dirname "$BUILD_POSTINSTALL_MARKER")"
touch "$BUILD_POSTINSTALL_MARKER"
