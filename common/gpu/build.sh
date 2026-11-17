#!/bin/bash
# Shared NVIDIA CC build operations.

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
