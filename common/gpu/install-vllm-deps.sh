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
  [[ -x /root/.venv/bin/python && -x /root/.venv/bin/vllm ]] || return 1
  VLLM_VERSION="$VLLM_VERSION" /root/.venv/bin/python - <<'PY' || return 1
import importlib.metadata
import os
raise SystemExit(0 if importlib.metadata.version("vllm") == os.environ["VLLM_VERSION"] else 1)
PY
  if [[ "${VLLM_WITH_MODELSCOPE:-0}" == 1 ]]; then
    [[ -x /root/.venv/bin/ninja ]] || return 1
    python3.11 -c 'import modelscope' || return 1
  fi
}
restore_vllm_build_headers
if vllm_deps_ready; then
  cleanup_package_caches
  exit 0
fi
python3.11 -m pip --version >/dev/null
packages=(uv importlib-metadata)
[[ "${VLLM_WITH_MODELSCOPE:-0}" != 1 ]] || packages+=('modelscope>=1.15.0')
python3.11 -m pip install -i "$PYPI_INDEX_URL" "${packages[@]}"
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
