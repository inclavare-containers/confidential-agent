#!/bin/bash
set -euo pipefail
if [[ -f /usr/local/libexec/confidential-agent/software-versions.sh ]]; then
    source /usr/local/libexec/confidential-agent/software-versions.sh
elif [[ -n "${ROOT_DIR:-}" && -f "$ROOT_DIR/common/software-versions.sh" ]]; then
    source "$ROOT_DIR/common/software-versions.sh"
else
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/common/software-versions.sh"
fi

OPENCLAW_USER="${1:?usage: install-openclaw-runtime.sh <openclaw-user> <openclaw-user-home>}"
OPENCLAW_USER_HOME="${2:?missing OpenClaw user home}"
OPENCLAW_CONFIG_DIR="${OPENCLAW_USER_HOME%/}/.openclaw"
OPENCLAW_VERSION="${OPENCLAW_VERSION:-$(ca_software_version openclaw)}"
OPENCLAW_NODE_VERSION="${OPENCLAW_NODE_VERSION:-$(ca_software_version node)}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

install -d -m 1777 /tmp
npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com/}"

ensure_openclaw_dirs() {
    getent group openclaw >/dev/null 2>&1 || groupadd -r openclaw
    if ! id -u "$OPENCLAW_USER" >/dev/null 2>&1; then
        useradd -r -g openclaw -d "$OPENCLAW_USER_HOME" -m -s /bin/bash "$OPENCLAW_USER"
    fi
    if [[ "$OPENCLAW_USER" != "root" ]]; then
        install -d -m 0750 -o "$OPENCLAW_USER" -g openclaw "$OPENCLAW_USER_HOME"
    fi
    install -d -m 0750 -o "$OPENCLAW_USER" -g openclaw "$OPENCLAW_CONFIG_DIR"
    install -d -m 0755 -o "$OPENCLAW_USER" -g openclaw \
        "$OPENCLAW_CONFIG_DIR/skills" \
        "$OPENCLAW_CONFIG_DIR/extensions"
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
                timeout "$timeout_sec" n "$node_version" && return 0
            else
                n "$node_version" && return 0
            fi
            delay=$((attempt * 15))
            echo "Node.js $node_version install attempt $attempt from $mirror failed; retrying in ${delay}s" >&2
            sleep "$delay"
        done
    done
    echo "failed to install Node.js $node_version after trying configured mirrors" >&2
    return 1
}

ensure_node22() {
    local n_bin
    if command -v node >/dev/null 2>&1 && [[ "$(node --version)" == "v$OPENCLAW_NODE_VERSION" ]] >/dev/null 2>&1; then
        return 0
    fi
    command -v tar >/dev/null 2>&1 || {
        echo "tar is required to install Node.js $OPENCLAW_NODE_VERSION" >&2
        exit 1
    }
    command -v xz >/dev/null 2>&1 || {
        echo "xz is required to install Node.js $OPENCLAW_NODE_VERSION" >&2
        exit 1
    }
    if ! n_bin="$(resolve_n_bin)"; then
        npm install -g n --no-audit --no-fund
        hash -r
        n_bin="$(resolve_n_bin || true)"
    fi
    if [[ -z "$n_bin" ]]; then
        echo "n was installed but its executable could not be found; npm prefix=$(npm prefix -g 2>/dev/null || true), npm root=$(npm root -g 2>/dev/null || true)" >&2
        exit 1
    fi
    if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
        install -d -m 0755 /usr/local/bin
        ln -sf "$n_bin" /usr/local/bin/n
        hash -r
        n_bin="$(command -v n 2>/dev/null || printf '%s' "$n_bin")"
    fi
    install_node_with_retry "$OPENCLAW_NODE_VERSION"
    export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
    hash -r
}

normalize_openclaw_bundled_manifest() {
    node - "$1" "$2" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const [manifestPath, openclawRoot] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const workspaceDeps = Object.entries(manifest.dependencies || {})
  .filter(([, version]) => typeof version === 'string' && version.startsWith('workspace:'));
if (workspaceDeps.length) {
  // Published extensions contain compiled entrypoints. Workspace references left
  // in their manifests are removable only when the published code bundles them.
  const entries = manifest.openclaw?.extensions || [];
  if (!entries.length || entries.some(entry => !/\.(?:[cm]?js)$/.test(entry)
      || !fs.statSync(path.resolve(path.dirname(manifestPath), entry), {throwIfNoEntry: false})?.isFile())) {
    throw new Error(`cannot verify compiled OpenClaw extension: ${manifestPath}`);
  }
  const artifacts = [];
  function collect(directory) {
    for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
      const filename = path.join(directory, entry.name);
      if (entry.isDirectory() && entry.name !== 'node_modules') collect(filename);
      else if (entry.isFile() && /\.(?:[cm]?js)$/.test(entry.name)) artifacts.push(filename);
    }
  }
  collect(path.join(openclawRoot, 'dist'));
  for (const [name] of workspaceDeps) {
    const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const reference = new RegExp("(['\"`])" + escaped + "(?:/[^'\"`\\r\\n]*)?\\1");
    for (const filename of artifacts) {
      if (reference.test(fs.readFileSync(filename, 'utf8'))) {
        throw new Error(`published OpenClaw code still references workspace dependency ${name}: ${filename}`);
      }
    }
  }
  for (const [name] of workspaceDeps) delete manifest.dependencies[name];
}
if (Object.values(manifest.devDependencies || {}).some(version =>
    typeof version === 'string' && version.startsWith('workspace:'))) delete manifest.devDependencies;
fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
NODE
}

preinstall_openclaw_bundled_runtime_deps() {
    local openclaw_root extensions_dir
    openclaw_root="$(npm root -g)/openclaw"
    extensions_dir="$openclaw_root/dist/extensions"
    [[ -d "$extensions_dir" ]] || return 0

    while IFS= read -r -d '' package_json; do
        local plugin_dir
        plugin_dir="$(dirname "$package_json")"
        jq -e '(.dependencies // {}) | length > 0' "$package_json" >/dev/null || continue
        (
            cd "$plugin_dir"
            if [[ -d node_modules ]]; then
                exit 0
            fi
            normalize_openclaw_bundled_manifest package.json "$openclaw_root"
            npm install --omit=dev --ignore-scripts --no-audit --no-fund
        )
    done < <(find "$extensions_dir" -mindepth 2 -maxdepth 2 -name package.json -print0 | sort -z)
}

install_dingtalk_extension() {
    local version integrity
    version="${DINGTALK_VERSION:-$(ca_software_version dingtalk)}"
    integrity="$(ca_software_version dingtalk_integrity)"
    local command=()
    if [[ "$OPENCLAW_USER" != root ]]; then
        command=(runuser -u "$OPENCLAW_USER" --)
    fi
    command+=(env "HOME=$OPENCLAW_USER_HOME" "OPENCLAW_STATE_DIR=$OPENCLAW_CONFIG_DIR"
        "OPENCLAW_CONFIG_PATH=$OPENCLAW_CONFIG_DIR/openclaw.json"
        "npm_config_registry=${NPM_REGISTRY:-https://registry.npmmirror.com/}" "$OPENCLAW_BIN")
    if [[ ! -d "$OPENCLAW_CONFIG_DIR/extensions/dingtalk" ]]; then
        "${command[@]}" plugins install "clawhub:@soimy/dingtalk@$version" --no-enable --accept-capabilities
    fi
    if ! "${command[@]}" plugins inspect dingtalk --json |
        jq -e --arg integrity "$integrity" '.install.integrity == $integrity' >/dev/null; then
        echo "DingTalk package integrity does not match the checked-in pin" >&2
        return 1
    fi
}

ensure_openclaw_dirs
ensure_node22
[[ "$(node --version)" == "v$OPENCLAW_NODE_VERSION" ]]
command -v npm >/dev/null
if ! command -v openclaw >/dev/null 2>&1 ||
    [[ "$(node -p 'require(process.argv[1]).version' "$(npm root -g)/openclaw/package.json" 2>/dev/null || true)" != "$OPENCLAW_VERSION" ]]; then
    npm install -g "openclaw@$OPENCLAW_VERSION" --no-audit --no-fund
fi
[[ "$(node -p 'require(process.argv[1]).version' "$(npm root -g)/openclaw/package.json")" == "$OPENCLAW_VERSION" ]]
OPENCLAW_BIN="$(command -v openclaw)"
if [[ -z "$OPENCLAW_BIN" ]]; then
    echo "openclaw binary was not installed" >&2
    exit 1
fi
if [[ "$OPENCLAW_BIN" != "/usr/local/bin/openclaw" ]]; then
    ln -sf "$OPENCLAW_BIN" /usr/local/bin/openclaw
fi
OPENCLAW_GLOBAL_ROOT="$(npm root -g)/openclaw"
chmod -R a+rX "$OPENCLAW_GLOBAL_ROOT" || true
chmod a+rx "$OPENCLAW_BIN" "$(readlink -f "$OPENCLAW_BIN")" /usr/local/bin/openclaw || true
preinstall_openclaw_bundled_runtime_deps
install_dingtalk_extension
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh install-openclaw-plugin "$OPENCLAW_USER" "$OPENCLAW_CONFIG_DIR"
npm cache clean --force || true
