#!/usr/bin/env bash

detect_os_id() {
  local os_release="${CA_OS_RELEASE_FILE:-/etc/os-release}"
  if [[ -r "$os_release" ]]; then
    # shellcheck disable=SC1090
    . "$os_release"
    # The OpenAnolis edition of Alibaba Cloud Linux 3 ships with ID=centos
    # but carries ALINUX_MINOR_ID / VARIANT_ID=openanolis and the same
    # alinux3-* repos. Treat it as alinux so the one-click runs on it.
    if [[ "${ID:-}" != "alinux" && ( "${VARIANT_ID:-}" == "openanolis" || -n "${ALINUX_MINOR_ID:-}" ) ]]; then
      printf 'alinux\n'
    else
      printf '%s\n' "${ID:-unknown}"
    fi
  else
    printf 'unknown\n'
  fi
}

detect_os_version_id() {
  local os_release="${CA_OS_RELEASE_FILE:-/etc/os-release}"
  if [[ -r "$os_release" ]]; then
    # shellcheck disable=SC1090
    . "$os_release"
    printf '%s\n' "${VERSION_ID:-unknown}"
  else
    printf 'unknown\n'
  fi
}

require_alinux3() {
  local os_id os_version
  os_id="$(detect_os_id)"
  os_version="$(detect_os_version_id)"
  if [[ "$os_id" != "alinux" || "$os_version" != "3" ]]; then
    die "one-click currently supports Alibaba Cloud Linux 3 only; detected ID='$os_id' VERSION_ID='$os_version'"
  fi
}

package_manager() {
  if command -v dnf >/dev/null 2>&1; then
    printf 'dnf\n'
  elif command -v yum >/dev/null 2>&1; then
    printf 'yum\n'
  else
    return 1
  fi
}

install_packages_best_effort() {
  local pm="$1"
  shift
  if (("$#" == 0)); then
    return 0
  fi
  "$pm" install -y "$@"
}

needs_cargo_build() {
  [[ "${CA_PACKAGED_INSTALL:-0}" != "1" && "${CA_SKIP_CARGO_BUILD:-0}" != "1" ]]
}

needs_local_image_build() {
  case "${CA_MODE:-deploy-openclaw}" in
    deploy-openclaw)
      [[ "${CA_IMAGE_SOURCE:-prebuilt}" == "build" && "${CA_SKIP_BUILD:-0}" != "1" ]]
      ;;
    deploy-openclaw-vllm)
      [[ "${CA_SKIP_BUILD:-0}" != "1" ]]
      ;;
    deploy-vllm-inference)
      [[ -z "${CA_IMAGE_SPEC:-}" && "${CA_SKIP_BUILD:-0}" != "1" ]]
      ;;
    install-only)
      [[ "${CA_PACKAGED_INSTALL:-0}" != "1" || "${CA_IMAGE_SOURCE:-prebuilt}" == "build" ]]
      ;;
    *)
      return 1
      ;;
  esac
}

install_os_dependencies() {
  if [[ "${CA_DEPLOY_ONLY:-0}" == 1 ]]; then return; fi
  if [[ "${CA_SKIP_DEPS:-0}" == "1" ]]; then
    log "skipping OS dependency installation"
    return
  fi
  is_root || die "OS dependency installation requires root. Re-run as root or pass --skip-deps after installing prerequisites."

  local pm
  require_alinux3
  pm="$(package_manager)" || die "yum or dnf is required on Alibaba Cloud Linux"

  log "installing host dependencies with $pm"
  local common_packages=(
    ca-certificates
    curl
    findutils
    jq
    openssh-clients
    openssl
    python3.11
    tar
    util-linux
    xz
  )
  if needs_local_image_build; then
    common_packages+=(libguestfs-tools-c)
  fi
  if needs_cargo_build; then
    common_packages+=(
      gcc
      git
      glibc-devel
      make
      pkgconf-pkg-config
    )
  fi
  if [[ "$CA_HOST_CLIENT" == codex ]]; then
    common_packages+=(procps-ng)
  fi
  install_packages_best_effort "$pm" "${common_packages[@]}"

  if [[ -n "$CA_HOST_CLIENT" ]]; then
    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
      if ! install_packages_best_effort "$pm" nodejs npm; then
        warn "failed to install nodejs/npm; install them manually before installing the selected host client"
      fi
    fi
  fi

  if needs_cargo_build && ! command -v cargo >/dev/null 2>&1; then
    if ! install_packages_best_effort "$pm" cargo rust; then
      warn "failed to install cargo/rust from OS repositories; install them manually or pass --allow-rustup"
    fi
  fi
}

host_node22_usable() {
  command -v node >/dev/null 2>&1 || return 1
  [[ "$(node --version)" == "v${CA_NODE_VERSION:-$(ca_software_version node)}" ]] >/dev/null 2>&1
}

resolve_n_bin() {
  local candidate npm_prefix npm_root
  candidate="$(command -v n 2>/dev/null || true)"
  if [[ -n "$candidate" ]]; then
    printf '%s\n' "$candidate"
    return 0
  fi
  npm_prefix="$(npm prefix -g 2>/dev/null || true)"
  npm_root="$(npm root -g 2>/dev/null || true)"
  for candidate in \
    "$npm_prefix/bin/n" \
    "$npm_root/n/bin/n" \
    /usr/local/bin/n \
    /usr/bin/n; do
    if [[ -f "$candidate" ]]; then
      chmod 0755 "$candidate" || true
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  candidate="$(find /usr/local /usr -path '*/node_modules/n/bin/n' -type f -print -quit 2>/dev/null || true)"
  if [[ -n "$candidate" ]]; then
    chmod 0755 "$candidate" || true
    printf '%s\n' "$candidate"
    return 0
  fi
  return 1
}

install_node_with_retry() {
  local node_version="$1"
  local n_bin="$2"
  local attempt delay mirror mirrors timeout_sec
  timeout_sec="${NODE_INSTALL_TIMEOUT_SEC:-300}"
  if [[ -n "${N_NODE_MIRROR:-}" ]]; then
    mirrors=("$N_NODE_MIRROR")
  else
    mirrors=("https://npmmirror.com/mirrors/node" "https://nodejs.org/dist")
  fi
  for mirror in "${mirrors[@]}"; do
    export N_NODE_MIRROR="$mirror"
    for attempt in 1 2 3; do
      rm -rf "/usr/local/n/versions/node/$node_version"
      if command -v timeout >/dev/null 2>&1; then
        timeout "$timeout_sec" "$n_bin" "$node_version" && return 0
      else
        "$n_bin" "$node_version" && return 0
      fi
      delay=$((attempt * 15))
      warn "Node.js $node_version install attempt $attempt from $mirror failed; retrying in ${delay}s"
      sleep "$delay"
    done
  done
  die "failed to install Node.js $node_version after trying configured mirrors"
}

ensure_host_node22() {
  if host_node22_usable; then
    return
  fi
  is_root || die "installing Node.js ${CA_NODE_VERSION:-24.21.0} requires root"
  command -v npm >/dev/null 2>&1 || die "npm is required to install Node.js ${CA_NODE_VERSION:-24.21.0}; rerun without --skip-deps or install npm manually"
  npm config set registry "${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  require_cmd tar
  require_cmd xz
  local n_bin
  if ! n_bin="$(resolve_n_bin)"; then
    npm install -g n --no-audit --no-fund
    hash -r
    n_bin="$(resolve_n_bin || true)"
  fi
  [[ -n "$n_bin" ]] || die "n was installed but its executable could not be found"
  if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
    install -d -m 0755 /usr/local/bin
    ln -sf "$n_bin" /usr/local/bin/n
    hash -r
  fi
  install_node_with_retry "${CA_NODE_VERSION:-24.21.0}" "$n_bin"
  export PATH=/usr/local/bin:/usr/local/sbin:$PATH
  hash -r
  host_node22_usable || die "Node.js ${CA_NODE_VERSION:-24.21.0} is not usable after installation"
}

npm_global_package_version() {
  local pkg_name="$1"
  local npm_root package_json
  npm_root="$(npm root -g 2>/dev/null || true)"
  package_json="$npm_root/$pkg_name/package.json"
  [[ -f "$package_json" ]] || return 1
  node -e 'console.log(require(process.argv[1]).version)' "$package_json"
}

openclaw_global_package_version() {
  npm_global_package_version openclaw
}

ensure_host_openclaw_runtime() {
  if [[ "${CA_SKIP_HOST_OPENCLAW:-0}" == "1" || "${CA_SKIP_HOST_CLIENT:-0}" == "1" ]]; then
    return 0
  fi
  if command -v openclaw >/dev/null && [[ "$(openclaw_global_package_version)" == "$CA_OPENCLAW_VERSION" ]]; then return; fi
  is_root || die "installing host OpenClaw CLI requires root"
  ensure_host_node22
  command -v npm >/dev/null 2>&1 || die "npm is required to install OpenClaw"
  npm config set registry "${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"

  local package_version npm_root openclaw_bin cli_version
  package_version="$(openclaw_global_package_version || true)"
  if [[ "$package_version" != "$CA_OPENCLAW_VERSION" ]]; then
    log "installing host OpenClaw CLI $CA_OPENCLAW_VERSION"
    npm install -g "openclaw@$CA_OPENCLAW_VERSION" --no-audit --no-fund
  fi

  npm_root="$(npm root -g)"
  openclaw_bin="$npm_root/openclaw/openclaw.mjs"
  [[ -f "$openclaw_bin" ]] || die "OpenClaw was installed but $openclaw_bin is missing"
  chmod 0755 "$openclaw_bin" || true
  install -d -m 0755 /usr/local/bin
  ln -sf "$openclaw_bin" /usr/local/bin/openclaw
  hash -r
  cli_version="$(openclaw --version 2>/dev/null | awk '{print $2}' || true)"
  [[ "$cli_version" == "$CA_OPENCLAW_VERSION" ]] || die "host OpenClaw version mismatch: expected $CA_OPENCLAW_VERSION, got ${cli_version:-unknown}"
  log "host OpenClaw CLI is ready: $(command -v openclaw) ($cli_version)"
}

ensure_host_access_client() {
  local client="$CA_HOST_CLIENT" package version
  case "$client" in
    '') return ;;
    openclaw) ensure_host_openclaw_runtime; return ;;
    codex) package=@openai/codex ;;
    opencode) package=opencode-ai ;;
  esac
  version="$(ca_software_version "$client")"
  if [[ "$(npm_global_package_version "$package")" == "$version" ]] && command -v "$client" >/dev/null; then return; fi
  ensure_host_node22
  npm install --global --registry "$CA_NPM_REGISTRY" --no-audit --no-fund "$package@$version"
  "$client" --version
}

rust_toolchain_usable() {
  command -v cargo >/dev/null 2>&1 || return 1
  cargo --version >/dev/null 2>&1 || return 1
  command -v rustc >/dev/null 2>&1 || return 1
  rustc --version >/dev/null 2>&1 || return 1
}

configure_cargo_mirror() {
  local config_dir="${CARGO_HOME:-${HOME:-/root}/.cargo}"
  local config_path="$config_dir/config.toml"
  local registry="${CA_CARGO_REGISTRY:-sparse+https://mirrors.aliyun.com/crates.io-index/}"
  [[ "${CA_CONFIGURE_CARGO_MIRROR:-1}" == "1" ]] || return
  if [[ -f "$config_path" ]] && grep -Eq '^\[source\.crates-io\]|^\[registries\.crates-io\]' "$config_path"; then
    log "leaving existing Cargo registry config unchanged: $config_path"
    return
  fi
  install -d -m 0755 "$config_dir"
  cat >>"$config_path" <<EOF

[source.crates-io]
replace-with = 'aliyun'

[source.aliyun]
registry = "$registry"
EOF
  log "configured Cargo crates.io mirror: $registry"
}

ensure_rust_toolchain() {
  if rust_toolchain_usable; then
    configure_cargo_mirror
    return
  fi
  if is_root; then
    local pm=""
    pm="$(package_manager || true)"
    if [[ -n "$pm" ]]; then
      log "installing Rust toolchain from OS repositories with $pm"
      install_packages_best_effort "$pm" cargo rust || true
      if rust_toolchain_usable; then
        configure_cargo_mirror
        return
      fi
      warn "OS repository Rust toolchain is not usable"
    fi
  fi
  if [[ "${CA_SKIP_RUSTUP:-0}" == "1" ]]; then
    die "cargo/rustc are missing or unusable. Install cargo/rust from Alibaba Cloud Linux 3 repositories, or pass --allow-rustup to permit the slower rustup fallback."
  fi
  require_cmd curl
  log "installing Rust with rustup"
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="${HOME:-/root}/.cargo/bin:$PATH"
  command -v cargo >/dev/null 2>&1 || die "cargo is still missing after rustup installation"
  configure_cargo_mirror
}
