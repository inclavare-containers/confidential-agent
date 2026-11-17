#!/bin/bash
set -euo pipefail

VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.28.0}"
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
NVIDIA_DRIVER_VERSION="${NVIDIA_DRIVER_VERSION:-580.82.07}"
NVIDIA_DRIVER_URL="${NVIDIA_DRIVER_URL:-https://cn.download.nvidia.cn/tesla/${NVIDIA_DRIVER_VERSION}/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run}"
NVIDIA_DRIVER_REFERER="${NVIDIA_DRIVER_REFERER:-https://www.nvidia.cn/}"
NVIDIA_DRIVER_SHA256="${NVIDIA_DRIVER_SHA256:-}"
CUDA_TOOLKIT_VERSION="${OPENCLAW_VLLM_CUDA_TOOLKIT_VERSION:-13.0.1}"
CUDA_BUNDLED_DRIVER_VERSION="${OPENCLAW_VLLM_CUDA_BUNDLED_DRIVER_VERSION:-580.82.07}"
CUDA_TOOLKIT_URL="${OPENCLAW_VLLM_CUDA_TOOLKIT_URL:-https://developer.download.nvidia.cn/compute/cuda/${CUDA_TOOLKIT_VERSION}/local_installers/cuda_${CUDA_TOOLKIT_VERSION}_${CUDA_BUNDLED_DRIVER_VERSION}_linux.run}"
source /usr/local/libexec/confidential-agent/gpu-build.sh

export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
BUILD_POSTINSTALL_MARKER=/var/lib/cai/vllm-inference/build-postinstall.done

if [[ -f "$BUILD_POSTINSTALL_MARKER" ]]; then
  echo "vLLM inference build postinstall already completed; skipping repeated mkosi postinstall pass"
  exit 0
fi

ensure_build_dependencies() {
  local missing=()
  for cmd in curl gcc git gocryptfs jq make python3.11 rpm s3fs sha256sum wget; do
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

install -d -m 0755 /usr/local/share/cai /var/lib/cai

ensure_build_dependencies
install -d -m 0755 /etc/modprobe.d
cat >/etc/modprobe.d/blacklist-nouveau.conf <<'EOF'
blacklist nouveau
options nouveau modeset=0
EOF

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
export VLLM_WITH_MODELSCOPE=0
exec bash /usr/local/libexec/confidential-agent/install-vllm-deps.sh
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
test -x /root/.venv/bin/python
test -x /root/.venv/bin/vllm
EOF
chmod 0755 /usr/local/bin/cai-vllm-setup.sh

# --- native s3fs + gocryptfs decryption stack ---
# The image's declared packages and EPEL repository provide both tools.
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
  --served-model-name "$VLLM_SERVED_MODEL_NAME" \
  --max-num-seqs 512
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
Environment=PATH=/root/.venv/bin:/usr/local/cuda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
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
