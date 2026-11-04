#!/bin/bash
set -euo pipefail
if [[ -f /usr/local/libexec/confidential-agent/software-versions.sh ]]; then
    source /usr/local/libexec/confidential-agent/software-versions.sh
elif [[ -n "${ROOT_DIR:-}" && -f "$ROOT_DIR/common/software-versions.sh" ]]; then
    source "$ROOT_DIR/common/software-versions.sh"
else
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/common/software-versions.sh"
fi
umask 022

HERMES_BRANCH="${HERMES_BRANCH:-main}"
HERMES_COMMIT="${HERMES_COMMIT:-$(ca_software_version hermes)}"
HERMES_PAYLOAD_DIR="${HERMES_PAYLOAD_DIR:-/usr/local/lib/cai-hermes}"
HERMES_INSTALL_DIR="${HERMES_INSTALL_DIR:-$HERMES_PAYLOAD_DIR/repo}"
HERMES_INSTALL_HOME="${HERMES_INSTALL_HOME:-$HERMES_PAYLOAD_DIR/home}"
HERMES_HOME="${HERMES_HOME:-/opt/data}"
HERMES_USER="${HERMES_USER:-hermes}"
HERMES_UID="${HERMES_UID:-10000}"
HERMES_GID="${HERMES_GID:-10000}"
HERMES_GIT_TIMEOUT_SEC="${HERMES_GIT_TIMEOUT_SEC:-600}"
HERMES_DOWNLOAD_CONNECT_TIMEOUT_SEC="${HERMES_DOWNLOAD_CONNECT_TIMEOUT_SEC:-20}"
HERMES_DOWNLOAD_TIMEOUT_SEC="${HERMES_DOWNLOAD_TIMEOUT_SEC:-180}"
HERMES_PYPI_INDEX_URL="${HERMES_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple/}"
HERMES_PYPI_ARTIFACT_MIRROR="${HERMES_PYPI_ARTIFACT_MIRROR-https://mirrors.aliyun.com/pypi/packages/}"
HERMES_UV_HTTP_TIMEOUT="${HERMES_UV_HTTP_TIMEOUT:-120}"
HERMES_UV_HTTP_RETRIES="${HERMES_UV_HTTP_RETRIES:-5}"
HERMES_NPM_REGISTRY_URL="${HERMES_NPM_REGISTRY_URL:-https://registry.npmmirror.com/}"
HERMES_NPM_FETCH_RETRIES="${HERMES_NPM_FETCH_RETRIES:-5}"
HERMES_NPM_FETCH_RETRY_MINTIMEOUT="${HERMES_NPM_FETCH_RETRY_MINTIMEOUT:-20000}"
HERMES_NPM_FETCH_RETRY_MAXTIMEOUT="${HERMES_NPM_FETCH_RETRY_MAXTIMEOUT:-120000}"
REPO_PATH="NousResearch/hermes-agent"
REPO_URL="https://github.com/NousResearch/hermes-agent.git"

github_proxy_url() {
    local url="$1"
    local proxy="${CA_GITHUB_PROXY_URL:-https://gh-proxy.org/}"

    case "$url" in
        https://github.com/*)
            printf '%s/%s\n' "${proxy%/}" "$url"
            ;;
        *)
            return 1
            ;;
    esac
}

github_proxy_prefix() {
    github_proxy_url "https://github.com/"
}

alternate_github_route() {
    if [[ "$1" == "$REPO_URL" ]]; then
        github_proxy_url "$REPO_URL"
    else
        printf '%s\n' "$REPO_URL"
    fi
}

git_with_timeout() {
    local git_env=(
        "GIT_HTTP_LOW_SPEED_LIMIT=${GIT_HTTP_LOW_SPEED_LIMIT:-1024}"
        "GIT_HTTP_LOW_SPEED_TIME=${GIT_HTTP_LOW_SPEED_TIME:-120}"
    )
    if command -v timeout >/dev/null 2>&1; then
        env "${git_env[@]}" timeout "$HERMES_GIT_TIMEOUT_SEC" git "$@"
    else
        env "${git_env[@]}" git "$@"
    fi
}

git_fetch_with_fallback() {
    local dir="$1"
    local ref="$2"
    shift 2
    local primary
    local fallback

    primary="$(git -C "$dir" remote get-url origin)" || return 1
    if git_with_timeout -C "$dir" fetch --no-tags --filter=blob:none "$@" origin "$ref"; then
        return 0
    fi

    fallback="$(alternate_github_route "$primary")" || return 1
    echo "Hermes source fetch failed; retrying the alternate GitHub route" >&2
    git -C "$dir" remote set-url origin "$fallback"
    git_with_timeout -C "$dir" fetch --no-tags --filter=blob:none "$@" origin "$ref"
}

clone_hermes_source() {
    local primary="$REPO_URL"
    local fallback

    rm -rf "$HERMES_INSTALL_DIR"
    install -d -m 0755 "$(dirname "$HERMES_INSTALL_DIR")"

    # Fetch the tip first, then acquire the pinned ancestry before checkout.
    # Downloading all historical trees up front makes a fresh install fragile
    # on slow routes even when the requested source revision is already known.
    if git_with_timeout clone --no-tags --filter=blob:none --depth 1 --single-branch --no-checkout --branch "$HERMES_BRANCH" "$primary" "$HERMES_INSTALL_DIR"; then
        return 0
    fi

    fallback="$(github_proxy_url "$primary")"
    echo "Direct GitHub clone failed for ${REPO_PATH}; retrying via ${fallback}" >&2
    rm -rf "$HERMES_INSTALL_DIR"
    git_with_timeout clone --no-tags --filter=blob:none --depth 1 --single-branch --no-checkout --branch "$HERMES_BRANCH" "$fallback" "$HERMES_INSTALL_DIR"
}

pin_hermes_commit() {
    local checkout=(checkout "$HERMES_BRANCH")
    if [[ -n "$HERMES_COMMIT" ]]; then
        git_fetch_with_fallback "$HERMES_INSTALL_DIR" "$HERMES_COMMIT" --depth 1 || return 1
        if ! git -C "$HERMES_INSTALL_DIR" merge-base --is-ancestor "$HERMES_COMMIT" "origin/$HERMES_BRANCH"; then
            local branch_ref
            branch_ref="+refs/heads/$HERMES_BRANCH:refs/remotes/origin/$HERMES_BRANCH"
            # Start just before the pin instead of downloading years of merged
            # history and unrelated release tags. Git still proves ancestry;
            # unusual commit dates fall back to a bounded history extension.
            local pinned_epoch
            pinned_epoch="$(git -C "$HERMES_INSTALL_DIR" show -s --format=%ct "$HERMES_COMMIT")" || return 1
            git_fetch_with_fallback "$HERMES_INSTALL_DIR" "$branch_ref" "--shallow-since=@$((pinned_epoch - 86400))" || true
            if ! git -C "$HERMES_INSTALL_DIR" merge-base --is-ancestor "$HERMES_COMMIT" "origin/$HERMES_BRANCH"; then
                git_fetch_with_fallback "$HERMES_INSTALL_DIR" "$branch_ref" --deepen=4096 || return 1
            fi
            if ! git -C "$HERMES_INSTALL_DIR" merge-base --is-ancestor "$HERMES_COMMIT" "origin/$HERMES_BRANCH"; then
                if [[ "$(git -C "$HERMES_INSTALL_DIR" rev-parse --is-shallow-repository)" == true ]]; then
                    git_fetch_with_fallback "$HERMES_INSTALL_DIR" "$branch_ref" --unshallow || return 1
                fi
            fi
            git -C "$HERMES_INSTALL_DIR" merge-base --is-ancestor "$HERMES_COMMIT" "origin/$HERMES_BRANCH" || {
                echo "Hermes commit $HERMES_COMMIT is not on branch $HERMES_BRANCH" >&2
                return 1
            }
        fi
        checkout=(checkout --detach "$HERMES_COMMIT")
    fi
    if git_with_timeout -C "$HERMES_INSTALL_DIR" "${checkout[@]}"; then
        return 0
    fi
    # A blobless clone can succeed before checkout needs another network fetch.
    # Apply the same fallback and deadline to that materialization step.
    local current alternate
    current="$(git -C "$HERMES_INSTALL_DIR" remote get-url origin)" || return 1
    alternate="$(alternate_github_route "$current")" || return 1
    git -C "$HERMES_INSTALL_DIR" remote set-url origin "$alternate"
    git_with_timeout -C "$HERMES_INSTALL_DIR" "${checkout[@]}"
}

hermes_entrypoint() {
    local candidate
    for candidate in "$HERMES_INSTALL_DIR/.hermes/bin/hermes" "$HERMES_INSTALL_DIR/venv/bin/hermes"; do
        if [[ -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

hermes_payload_installed() {
    test -d "$HERMES_INSTALL_DIR/.git" && hermes_entrypoint >/dev/null || return 1
    if [[ -f "$HERMES_INSTALL_DIR/pm/environments.py" ]]; then
        test -s "$HERMES_PAYLOAD_DIR/manifest.json" || return 1
    fi
}

installed_ref_matches() {
    local actual branch expected

    hermes_payload_installed || return 1
    actual="$(git -C "$HERMES_INSTALL_DIR" rev-parse HEAD 2>/dev/null)" || return 1

    if [[ -n "$HERMES_COMMIT" ]]; then
        expected="$(git -C "$HERMES_INSTALL_DIR" rev-parse "$HERMES_COMMIT^{commit}" 2>/dev/null || printf '%s' "$HERMES_COMMIT")"
        [[ "$actual" == "$expected" || "$actual" == "$HERMES_COMMIT" ]]
        return
    fi

    branch="$(git -C "$HERMES_INSTALL_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)" || return 1
    [[ "$branch" == "$HERMES_BRANCH" ]]
}

run_official_installer_with_github_proxy() {
    local installer="$1"
    shift

    local installer_support git_config curl_bin saved_path
    installer_support="$(mktemp -d)"
    git_config="$installer_support/gitconfig"
    curl_bin="$(command -v curl)"
    saved_path="$PATH"
    mkdir "$installer_support/bin"
    cat > "$installer_support/bin/curl" <<EOF
#!/bin/bash
exec "$curl_bin" --connect-timeout "$HERMES_DOWNLOAD_CONNECT_TIMEOUT_SEC" --max-time "$HERMES_DOWNLOAD_TIMEOUT_SEC" "\$@"
EOF
    chmod 0755 "$installer_support/bin/curl"
    export PATH="$installer_support/bin:$PATH"
    local source_url
    source_url="$(git -C "$HERMES_INSTALL_DIR" remote get-url origin 2>/dev/null || true)"
    if [[ "$source_url" == "$REPO_URL" ]]; then
        : > "$git_config"
    else
        git config --file "$git_config" url."$(github_proxy_prefix)".insteadOf "https://github.com/"
    fi
    export GIT_CONFIG_GLOBAL="$git_config"
    export GIT_TERMINAL_PROMPT=0
    export GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}"
    export GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-120}"

    set +e
    "$installer" "$@"
    local status=$?
    set -e

    export PATH="$saved_path"
    rm -rf "$installer_support"
    unset GIT_CONFIG_GLOBAL
    return "$status"
}

ensure_hermes_user() {
    if ! getent group "$HERMES_USER" >/dev/null 2>&1; then
        groupadd -g "$HERMES_GID" "$HERMES_USER"
    fi
    if ! id -u "$HERMES_USER" >/dev/null 2>&1; then
        useradd -u "$HERMES_UID" -g "$HERMES_USER" -d "$HERMES_HOME" -s /sbin/nologin "$HERMES_USER"
    fi
}

prepare_hermes_locked_downloads() {
    [[ -n "$HERMES_PYPI_ARTIFACT_MIRROR" ]] || return 0
    python3.11 - "$HERMES_INSTALL_DIR" "$HERMES_PAYLOAD_DIR" "$HERMES_PYPI_ARTIFACT_MIRROR" <<'PY'
import copy,hashlib,json,re,sys,tomllib
from pathlib import Path
from urllib.parse import urlsplit
repo,payload = (Path(value) for value in sys.argv[1:3])
mirror = sys.argv[3].rstrip('/') + '/'
parts = urlsplit(mirror)
assert parts.scheme == 'https' and parts.hostname and not parts.query and not parts.fragment, 'invalid PyPI artifact mirror'
origin = 'https://files.pythonhosted.org/packages/'
changes = []
records = {}
for relative in ['uv.lock', 'pm/uv.lock']:
    filename = repo / relative
    original = filename.read_text()
    expected = copy.deepcopy(tomllib.loads(original))
    count = 0
    def remap(value):
        global count
        if isinstance(value, dict):
            if isinstance(value.get('url'), str) and value['url'].startswith(origin):
                assert re.fullmatch(r'sha256:[a-f0-9]{64}', value.get('hash', '')), 'unhashed locked artifact'
                value['url'] = mirror + value['url'][len(origin):]
                count += 1
            for nested in value.values(): remap(nested)
        elif isinstance(value, list):
            for nested in value: remap(nested)
    remap(expected)
    rewritten = original.replace(origin, mirror)
    # Only artifact locations may change; versions, hashes, registry identity,
    # dependency edges and markers remain exactly as the pinned source records.
    assert tomllib.loads(rewritten) == expected, 'unexpected non-artifact lock change'
    changes.append((filename, rewritten))
    records[relative] = {'source_sha256': hashlib.sha256(original.encode()).hexdigest(),
                         'transport_sha256': hashlib.sha256(rewritten.encode()).hexdigest(),
                         'mirrored_artifacts': count}
for filename, rewritten in changes:
    filename.write_text(rewritten)
payload.mkdir(parents=True, exist_ok=True)
(payload / 'locked-downloads.json').write_text(json.dumps(records, indent=2) + '\n')
PY
}

prepare_hermes_binary_downloads() {
    # PM uses urllib rather than our curl/git wrappers for Python, FFmpeg and
    # other pinned tools. Change only its transport, retaining native verification.
    python3.11 - "$HERMES_INSTALL_DIR" "$HERMES_PAYLOAD_DIR" "${CA_GITHUB_PROXY_URL:-https://gh-proxy.org/}" <<'PY'
import hashlib,json,os,sys
from pathlib import Path
from urllib.parse import urlsplit
repo,payload = (Path(value) for value in sys.argv[1:3])
proxy = sys.argv[3].rstrip('/') + '/'
parts = urlsplit(proxy)
assert (parts.scheme == 'https' and parts.hostname and not parts.username
        and not parts.password and not parts.query and not parts.fragment
        and not any(c.isspace() or c == '\\' for c in proxy)), 'invalid GitHub proxy prefix'
filename = repo / 'pm/artifact_mirror.py'
original = filename.read_text()
needle = '    return Source(url, dest, sha256, fallbacks=() if url == archive else (archive,))'
marker = '    # CA: route pinned GitHub binaries through the configured proxy first.'
replacement = '\n'.join([
    marker,
    '    if url.startswith("https://github.com/"):',
    f'        return Source({proxy!r} + url, dest, sha256, fallbacks=tuple(dict.fromkeys((url, archive))))',
    needle,
])
# Repeating the same preparation is safe; an unfamiliar upstream adapter fails
# explicitly instead of bypassing its hash checks or silently ignoring the proxy.
source = original.replace(replacement, needle) if replacement in original else original
assert source.count(needle) == 1 and marker not in source, 'unsupported Hermes binary download adapter'
rewritten = source.replace(needle, replacement)
compile(rewritten, str(filename), 'exec')
if rewritten != original:
    temporary = filename.with_suffix('.ca-transport.tmp')
    temporary.write_text(rewritten)
    temporary.chmod(filename.stat().st_mode & 0o777)
    os.replace(temporary, filename)
payload.mkdir(parents=True, exist_ok=True)
record = {'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
          'transport_sha256': hashlib.sha256(rewritten.encode()).hexdigest(),
          'github_proxy': proxy, 'fallbacks': ['original-url', 'official-hash-mirror']}
(payload / 'binary-downloads.json').write_text(json.dumps(record, indent=2) + '\n')
PY
}

prepare_hermes_stripped_python() {
    [[ "${HERMES_COMPACT_RUNTIME:-0}" == "1" ]] || return 0
    # Select the upstream stripped archive before PM installs and seals it.
    # Never strip installed executables: that would invalidate PM's tree hashes
    # and can destroy the appended payload of bundled Bun executables.
    python3.11 - "$HERMES_INSTALL_DIR" "$HERMES_PAYLOAD_DIR" <<'PY'
import hashlib,json,os,platform,sys
from pathlib import Path
repo,payload = (Path(value) for value in sys.argv[1:])
assert sys.platform == 'linux' and platform.machine() == 'x86_64', 'compact Hermes Python is pinned for Linux x86_64'
filename = repo / 'pm/lock.json'
original = filename.read_bytes()
lock = json.loads(original)
python = lock['packages']['python']
assert python['version'] == '3.14.7+20260901', 'refresh the compact Python pin for this Hermes version'
base = 'https://github.com/astral-sh/python-build-standalone/releases/download/20260901/'
name = 'cpython-3.14.7+20260901-x86_64-unknown-linux-gnu-install_only'
source = {'url': base + name + '.tar.gz',
          'sha256': '0ab3305457051cd3e7c031857e005f1bda17c218a1990567dacaaac6dd1d14f0'}
replacement = {'url': base + name + '_stripped.tar.gz',
               'sha256': '3959f92825141e04adf44982d3a83ee57af0877e893b0796e04c1468749d9b04'}
artifact = python['artifacts']['linux-x64']
assert artifact in (source, replacement), 'unexpected upstream Python artifact; refusing to replace its pin'
if artifact == source:
    python['artifacts']['linux-x64'] = replacement
    temporary = filename.with_suffix('.compact.tmp')
    temporary.write_text(json.dumps(lock, indent=2) + '\n')
    temporary.chmod(filename.stat().st_mode & 0o777)
    os.replace(temporary, filename)
payload.mkdir(parents=True, exist_ok=True)
(payload / 'compact-python.json').write_text(json.dumps({
    'version': python['version'], 'source': source, 'selected': replacement,
    'lock_sha256': hashlib.sha256(filename.read_bytes()).hexdigest(),
}, indent=2) + '\n')
PY
}

run_hermes_installer_stages() {
    local installer="$1"
    shift
    local stages stage
    stages="$("$installer" --manifest | python3.11 -c '
import json,sys
manifest=json.load(sys.stdin)
assert manifest["protocol_version"] == 1
names=[stage["name"] for stage in manifest["stages"]]
assert len(names) == len(set(names)) and "repository" in names and "python-deps" in names
assert names.index("repository") < names.index("python-deps")
assert all(name.replace("-", "").isalnum() for name in names)
print("\n".join(names))
')"
    # Use the official stage protocol to select download transport after its
    # repository checkout, then execute every declared installation stage.
    while IFS= read -r stage; do
        run_official_installer_with_github_proxy "$installer" "$@" --stage "$stage"
        if [[ "$stage" == repository ]]; then
            prepare_hermes_locked_downloads
            prepare_hermes_binary_downloads
            prepare_hermes_stripped_python
        fi
    done <<< "$stages"
}

install_hermes() (
    local installer="$HERMES_INSTALL_DIR/scripts/install.sh"
    local args=(
        --dir "$HERMES_INSTALL_DIR"
        --hermes-home "$HERMES_INSTALL_HOME"
        --branch "$HERMES_BRANCH"
        --skip-browser
        --skip-setup
        --non-interactive
    )

    [[ -x "$installer" ]] || chmod +x "$installer"
    if [[ -n "$HERMES_COMMIT" ]]; then
        args+=(--commit "$HERMES_COMMIT")
    fi

    export UV_PYTHON_INSTALL_DIR="${UV_PYTHON_INSTALL_DIR:-/usr/local/share/uv/python}"
    export UV_PYTHON_BIN_DIR="${UV_PYTHON_BIN_DIR:-/usr/local/share/uv/bin}"
    export UV_PYTHON_INSTALL_MIRROR="${UV_PYTHON_INSTALL_MIRROR:-$(github_proxy_url https://github.com/astral-sh/python-build-standalone/releases/download)}"
    export UV_INDEX_URL="${UV_INDEX_URL:-${UV_DEFAULT_INDEX:-$HERMES_PYPI_INDEX_URL}}"
    export UV_HTTP_TIMEOUT="${UV_HTTP_TIMEOUT:-$HERMES_UV_HTTP_TIMEOUT}"
    export UV_HTTP_RETRIES="${UV_HTTP_RETRIES:-$HERMES_UV_HTTP_RETRIES}"
    export PIP_INDEX_URL="${PIP_INDEX_URL:-$HERMES_PYPI_INDEX_URL}"
    export NPM_CONFIG_REGISTRY="${NPM_CONFIG_REGISTRY:-$HERMES_NPM_REGISTRY_URL}"
    export NPM_CONFIG_FETCH_RETRIES="${NPM_CONFIG_FETCH_RETRIES:-$HERMES_NPM_FETCH_RETRIES}"
    export NPM_CONFIG_FETCH_RETRY_MINTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MINTIMEOUT:-$HERMES_NPM_FETCH_RETRY_MINTIMEOUT}"
    export NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT:-$HERMES_NPM_FETCH_RETRY_MAXTIMEOUT}"
    export PLAYWRIGHT_DOWNLOAD_HOST="${PLAYWRIGHT_DOWNLOAD_HOST:-https://npmmirror.com/mirrors/playwright}"
    export HERMES_HOME="$HERMES_INSTALL_HOME"
    export HERMES_RUNTIME_DIR="$HERMES_PAYLOAD_DIR/tools"
    run_hermes_installer_stages "$installer" "${args[@]}"
    seal_hermes_runtime
)

seal_hermes_runtime() {
    # Hermes PM selects dependencies per data home. Its supported payload
    # manifest lets every service use the verified image environment after
    # /opt/data is mounted, without another install or mutable shared state.
    [[ -f "$HERMES_INSTALL_DIR/pm/environments.py" ]] || return 0
    local hermes_bin runtime_python
    hermes_bin="$(hermes_entrypoint)"
    runtime_python="$("$hermes_bin" --print-runtime-command | python3.11 -c 'import json,sys; print(json.load(sys.stdin)[0])')"
    "$runtime_python" - "$HERMES_INSTALL_DIR" "$HERMES_PAYLOAD_DIR" <<'PY'
import json,sys
from pathlib import Path
repo,payload = (Path(value).resolve() for value in sys.argv[1:])
sys.path.insert(0, str(repo))
from pm.environments import committed_venv, store_root
venv = committed_venv(repo)
assert venv is not None and (venv / 'pyvenv.cfg').is_file(), 'Hermes dependencies are not committed'
store = store_root(repo)
manifest = {'repo': str(repo.relative_to(payload)), 'venv': str(venv.relative_to(payload)),
            'store': str(store.relative_to(payload)),
            'runtime': {'commands': {'hermes': str((repo / '.hermes/bin/hermes').relative_to(payload))}}}
(payload / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
stamp = repo / 'install-stamp.json'
metadata = json.loads(stamp.read_text())
metadata.update(updateMechanism='external', runtimeDir=str(store))
stamp.write_text(json.dumps(metadata, indent=2) + '\n')
PY
    rm -f "$HERMES_INSTALL_HOME/.env" "$HERMES_INSTALL_HOME/config.yaml"
    chmod -R a+rX "$HERMES_PAYLOAD_DIR"
}

install_hermes_shim() {
    local hermes_bin
    hermes_bin="$(hermes_entrypoint)" || {
        echo "Hermes entrypoint was not installed under $HERMES_INSTALL_DIR" >&2
        exit 1
    }

    install -d -m 0755 /usr/local/bin
    cat >/usr/local/bin/hermes <<EOF
#!/usr/bin/env bash
unset PYTHONPATH
unset PYTHONHOME
export HERMES_RUNTIME_DIR="$HERMES_PAYLOAD_DIR/tools"
export HERMES_DISABLE_LAZY_INSTALLS=1
exec "$hermes_bin" "\$@"
EOF
    chmod 0755 /usr/local/bin/hermes
}

command -v curl >/dev/null 2>&1 || {
    echo "curl is required to install Hermes Agent" >&2
    exit 1
}
command -v git >/dev/null 2>&1 || {
    echo "git is required to install Hermes Agent" >&2
    exit 1
}
command -v tar >/dev/null 2>&1 || {
    echo "tar is required to install Hermes Agent" >&2
    exit 1
}
command -v xz >/dev/null 2>&1 || {
    echo "xz is required to install Hermes Agent" >&2
    exit 1
}

ensure_hermes_user
if installed_ref_matches; then
    echo "Hermes Agent is already installed at $HERMES_INSTALL_DIR; skipping source install" >&2
else
    clone_hermes_source
    pin_hermes_commit
    install_hermes
fi
install_hermes_shim
if [[ "$HERMES_PAYLOAD_DIR" == /usr/local/lib/cai-hermes ]]; then
    ln -sTf "$HERMES_INSTALL_DIR" /usr/local/lib/hermes-agent
fi

test -x /usr/local/bin/hermes || {
    echo "Hermes command was not installed at /usr/local/bin/hermes" >&2
    exit 1
}

rm -f "$HERMES_HOME/.env" "$HERMES_HOME/config.yaml"
install -d -m 0700 -o "$HERMES_USER" -g "$HERMES_USER" "$HERMES_HOME"
for sub in cron sessions logs hooks memories skills skins plans workspace home pairing platforms/pairing; do
    install -d -m 0700 -o "$HERMES_USER" -g "$HERMES_USER" "$HERMES_HOME/$sub"
done
chown -R "$HERMES_USER:$HERMES_USER" "$HERMES_HOME"

install -d -m 0755 /usr/local/share/confidential-agent
{
    printf 'branch=%s\n' "$HERMES_BRANCH"
    if [[ -n "$HERMES_COMMIT" ]]; then
        printf 'commit=%s\n' "$HERMES_COMMIT"
    else
        git -C "$HERMES_INSTALL_DIR" rev-parse HEAD | sed 's/^/commit=/'
    fi
} >/usr/local/share/confidential-agent/hermes-install.txt

if [[ -f /etc/systemd/system/cai-hermes-agent.service ]]; then
    systemctl daemon-reload || true
    systemctl enable cai-hermes-agent.service
else
    echo "cai-hermes-agent.service is not installed; skipping systemd enable" >&2
fi
