#!/bin/sh
set -eu

DEFAULT_REPO="https://github.com/inclavare-containers/confidential-agent.git"
DEFAULT_BRANCH="master"

usage() {
    cat <<'EOF'
Usage:
  curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/master/one-click/install-openclaw-vllm.sh | sh
  sh one-click/install-openclaw-vllm.sh [deploy|install-only|cleanup] [options]

Bootstrap options:
  --repo URL             Git repository to clone when running through curl | sh
  --branch NAME          Git branch to checkout when running through curl | sh
  --ref REF              Git branch, tag, or commit to checkout when running through curl | sh
  --commit SHA           Alias for --ref; SHA must be the full 40 hex chars
  --source-dir PATH      Local source checkout directory
  --help                 Show this help

Deployment options:
  --install-host-client  Install the local OpenClaw CLI
  --skip-host-client     Skip the local CLI installation and prompt

All other options are handled by the OpenClaw vLLM one-click installer after checkout.
EOF
}

append_pass_arg() {
    printf '%s\0' "$1" >>"$pass_args_file"
}

validate_commit_ref() {
    case "$1" in
        ""|*[!0123456789abcdefABCDEF]*)
            echo "--commit requires a full 40-character hex commit SHA" >&2
            exit 2
            ;;
    esac
    if [ "${#1}" -ne 40 ]; then
        echo "--commit requires a full 40-character hex commit SHA, not a short SHA" >&2
        exit 2
    fi
}

run_main() {
    main_script="$1"
    export CA_ONE_CLICK_TARGET=openclaw-vllm
    if [ -s "$pass_args_file" ]; then
        xargs -0 -a "$pass_args_file" bash "$main_script"
        exit $?
    fi
    exec bash "$main_script"
}

ensure_git() {
    if command -v git >/dev/null 2>&1 && command -v git-lfs >/dev/null 2>&1; then
        git lfs install --system >/dev/null 2>&1 || git lfs install >/dev/null 2>&1 || true
        return 0
    fi
    if [ "$(id -u)" != "0" ]; then
        echo "git and git-lfs are required. Re-run as root or install git/git-lfs first." >&2
        exit 2
    fi
    if command -v dnf >/dev/null 2>&1; then
        dnf install -y git git-lfs ca-certificates
    elif command -v yum >/dev/null 2>&1; then
        yum install -y git git-lfs ca-certificates
    else
        echo "git and git-lfs are required and no yum/dnf package manager was found." >&2
        exit 2
    fi
    command -v git >/dev/null 2>&1 || { echo "git installation failed" >&2; exit 2; }
    command -v git-lfs >/dev/null 2>&1 || { echo "git-lfs installation failed" >&2; exit 2; }
    git lfs install --system >/dev/null 2>&1 || git lfs install >/dev/null 2>&1 || true
}

github_proxy_repo_url() {
    [ -n "${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}" ] || return 1
    case "$1" in
        https://github.com/*|https://raw.githubusercontent.com/*)
            proxy="${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}"
            case "$proxy" in
                */) printf '%s%s\n' "$proxy" "$1" ;;
                *) printf '%s/%s\n' "$proxy" "$1" ;;
            esac
            ;;
        *)
            return 1
            ;;
    esac
}

run_git_with_timeout() {
    timeout_sec="${CA_GIT_FETCH_TIMEOUT_SEC:-30}"
    if command -v timeout >/dev/null 2>&1; then
        GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
        GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
            timeout "$timeout_sec" git "$@"
    else
        GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
        GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
            git "$@"
    fi
}

github_proxy_first() {
    case "${CA_GITHUB_PROXY_FIRST:-1}" in
        1|true|TRUE|yes|YES|proxy|proxy-first)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

git_fetch_with_fallback() {
    dir="$1"
    repo_url="$2"
    ref_name="$3"
    proxy_url="$(github_proxy_repo_url "$repo_url" || true)"
    if github_proxy_first && [ -n "$proxy_url" ]; then
        git -C "$dir" remote set-url origin "$proxy_url"
        if run_git_with_timeout -C "$dir" fetch --depth 1 origin "$ref_name"; then
            return 0
        fi
        echo "GitHub proxy fetch failed, retrying original URL $repo_url" >&2
    fi
    git -C "$dir" remote set-url origin "$repo_url"
    if run_git_with_timeout -C "$dir" fetch --depth 1 origin "$ref_name"; then
        return 0
    fi
    if [ -z "$proxy_url" ]; then
        return 1
    fi
    echo "Direct GitHub fetch failed, retrying via $proxy_url" >&2
    git -C "$dir" remote set-url origin "$proxy_url"
    run_git_with_timeout -C "$dir" fetch --depth 1 origin "$ref_name"
}

git_lfs_pull_with_fallback() {
    dir="$1"
    repo_url="$2"
    timeout_sec="${CA_GIT_LFS_TIMEOUT_SEC:-300}"
    proxy_url="$(github_proxy_repo_url "$repo_url" || true)"
    current_url="$(git -C "$dir" remote get-url origin 2>/dev/null || printf '%s' "$repo_url")"

    git -C "$dir" remote set-url origin "$current_url"
    if command -v timeout >/dev/null 2>&1; then
        if GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
           GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
           timeout "$timeout_sec" git -C "$dir" lfs pull origin; then
            return 0
        fi
    elif GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
         GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
         git -C "$dir" lfs pull origin; then
        return 0
    fi

    if [ "$current_url" = "$proxy_url" ]; then
        echo "Git LFS proxy pull failed, retrying original URL $repo_url" >&2
        git -C "$dir" remote set-url origin "$repo_url"
    elif [ "$current_url" = "$repo_url" ] && [ -n "$proxy_url" ]; then
        echo "Git LFS pull failed, retrying via $proxy_url" >&2
        git -C "$dir" remote set-url origin "$proxy_url"
    elif github_proxy_first && [ -n "$proxy_url" ]; then
        echo "Git LFS pull failed, retrying via $proxy_url" >&2
        git -C "$dir" remote set-url origin "$proxy_url"
    elif [ "$current_url" != "$repo_url" ]; then
        echo "Git LFS pull failed, retrying original URL $repo_url" >&2
        git -C "$dir" remote set-url origin "$repo_url"
    else
        return 1
    fi
    if command -v timeout >/dev/null 2>&1; then
        GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
        GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
            timeout "$timeout_sec" git -C "$dir" lfs pull origin
    else
        GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}" \
        GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}" \
            git -C "$dir" lfs pull origin
    fi
}

checkout_source() {
    if [ ! -e "$source_dir/.git" ]; then
        mkdir -p "$source_dir"
        git -C "$source_dir" init
        git -C "$source_dir" remote add origin "$repo"
    fi
    git_fetch_with_fallback "$source_dir" "$repo" "$ref"
    GIT_LFS_SKIP_SMUDGE=1 git -C "$source_dir" checkout --detach FETCH_HEAD
}

launch_github_frontend() {
    slug="${1%.git}"
    raw="https://raw.githubusercontent.com/$slug/$ref"
    frontend_dir="$(mktemp -d "${TMPDIR:-/tmp}/ca-one-click-frontend.XXXXXX")"
    export CA_ONE_CLICK_TARGET=openclaw-vllm
    first="$raw"
    second="$(github_proxy_repo_url "$raw" || true)"
    if [ -n "$second" ] && github_proxy_first; then
        first="$second"
        second="$raw"
    fi
    if ! curl -fsSL --retry 2 --connect-timeout 10 --max-time 60 \
        -o "$frontend_dir/bootstrap.sh" "$first/one-click/lib/bootstrap-frontend.sh"; then
        [ -n "$second" ] || return 1
        curl -fsSL --retry 2 --connect-timeout 10 --max-time 60 \
            -o "$frontend_dir/bootstrap.sh" "$second/one-click/lib/bootstrap-frontend.sh"
    fi
    sh "$frontend_dir/bootstrap.sh" "$first" "$second" install-openclaw-vllm.sh "$pass_args_file"
}

bootstrap_runtime() {
    repo="$CA_BOOTSTRAP_REPO"
    ref="$CA_BOOTSTRAP_REF"
    source_dir="$CA_BOOTSTRAP_ROOT"
    ensure_git
    if [ "$CA_BOOTSTRAP_ACTION" = source ]; then
        checkout_source
    fi
    git_lfs_pull_with_fallback "$source_dir" "$repo"
}

main() {
    if [ "${CA_BOOTSTRAP_HOOK:-}" = "$0" ]; then
        bootstrap_runtime
        return
    fi
    explicit_ref=""
    if [ -n "${CA_ONE_CLICK_REPO:-}${CA_ONE_CLICK_REF:-}${CA_ONE_CLICK_BRANCH:-}" ]; then
        explicit_ref=1
    fi
    repo="${CA_ONE_CLICK_REPO:-$DEFAULT_REPO}"
    ref="${CA_ONE_CLICK_REF:-${CA_ONE_CLICK_BRANCH:-$DEFAULT_BRANCH}}"
    commit_ref=0
    source_dir="${CA_ONE_CLICK_SOURCE_DIR:-}"
    frontend_dir=""
    pass_args_file="$(mktemp "${TMPDIR:-/tmp}/ca-one-click-args.XXXXXX")"
    trap 'rm -f "$pass_args_file"; if [ -n "$frontend_dir" ]; then rm -rf -- "$frontend_dir"; fi' EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [ ! -t 0 ] && ( : </dev/tty ) 2>/dev/null; then
        exec </dev/tty
    fi

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --repo)
                [ "$#" -ge 2 ] || { echo "missing value for --repo" >&2; exit 2; }
                repo="$2"
                explicit_ref=1
                shift 2
                ;;
            --branch)
                [ "$#" -ge 2 ] || { echo "missing value for --branch" >&2; exit 2; }
                ref="$2"
                explicit_ref=1
                shift 2
                ;;
            --ref)
                [ "$#" -ge 2 ] || { echo "missing value for $1" >&2; exit 2; }
                ref="$2"
                explicit_ref=1
                shift 2
                ;;
            --commit)
                [ "$#" -ge 2 ] || { echo "missing value for --commit" >&2; exit 2; }
                ref="$2"
                commit_ref=1
                explicit_ref=1
                shift 2
                ;;
            --source-dir)
                [ "$#" -ge 2 ] || { echo "missing value for --source-dir" >&2; exit 2; }
                source_dir="$2"
                shift 2
                ;;
            --help|-h)
                usage
                exit 0
                ;;
            *)
                append_pass_arg "$1"
                shift
                ;;
        esac
    done

    if [ "$commit_ref" = "1" ]; then
        validate_commit_ref "$ref"
    fi

    script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P 2>/dev/null || pwd)
    local_root=$(CDPATH= cd -- "$script_dir/.." 2>/dev/null && pwd -P 2>/dev/null || true)
    if [ -z "$explicit_ref" ]; then
        if [ -n "$source_dir" ] && [ -f "$source_dir/one-click/lib/main.sh" ]; then
            run_main "$source_dir/one-click/lib/main.sh"
        fi
        if [ -z "$source_dir" ] && [ -n "$local_root" ] && [ -f "$local_root/Cargo.toml" ] && [ -f "$local_root/one-click/lib/main.sh" ]; then
            run_main "$local_root/one-click/lib/main.sh"
        fi
    fi

    source_dir="$(realpath -m -- "${source_dir:-${HOME:-/root}/.cache/confidential-agent/source}")"
    export CA_PACKAGED_INSTALL=0 CA_BOOTSTRAP_ROOT="$source_dir" CA_BOOTSTRAP_ACTION=source
    export CA_BOOTSTRAP_REPO="$repo" CA_BOOTSTRAP_REF="$ref"
    case "$repo" in
        https://github.com/*)
            launch_github_frontend "${repo#https://github.com/}"
            ;;
        *)
            checkout_source
            export CA_BOOTSTRAP_ACTION=lfs CA_BOOTSTRAP_HOOK="$source_dir/one-click/install-openclaw-vllm.sh"
            run_main "$source_dir/one-click/lib/main.sh"
            ;;
    esac
}

main "$@"
