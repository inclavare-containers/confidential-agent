#!/bin/sh
set -eu

DEFAULT_REPO="https://github.com/inclavare-containers/confidential-agent.git"
DEFAULT_BRANCH="master"
RELEASE_VERSION="0.3.1"
PACKAGED_DIR="/usr/share/cagent/one-click"

usage() {
    cat <<'EOF'
Usage:
  curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install.sh | sh
  sh one-click/install.sh [deploy|continue|install-only|cleanup] [options]

Bootstrap options:
  --repo URL             Git repository for source workflows
  --branch NAME          Git branch for source workflows
  --ref REF              Git branch, tag, or commit for source workflows
  --commit SHA           Alias for --ref; SHA must be the full 40 hex chars
  --source-dir PATH      Local source checkout directory
  --help                 Show this help

Deployment options:
  --install-host-client  Install the local application CLI for the selected access method
  --skip-host-client     Skip the local application CLI installation and prompt
  --harness NAME         openclaw (default), hermes, codex, claude-code, qodercli, opencode, pi, dsh
  --api-base-url URL     Inference API base URL; defaults to Bailian
  --api-key KEY          Inference API key (or CA_API_KEY); --bailian-api-key is an alias
  --model MODEL          Model id; --bailian-model is an alias
  --image-source MODE    prebuilt (default) or build
  --artifact PATH        Local Shelter artifact
  --image-release PATH   Local image-release.json with --release-digest
  --release-digest HASH  Trusted SHA-256 digest of --image-release
  --image-id ID          Existing regional ECS image ID
  --non-interactive      Fail if a required value is missing

The one-click installer handles all other deployment options.
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
    if [ -s "$pass_args_file" ]; then
        xargs -0 -a "$pass_args_file" bash "$main_script"
        exit $?
    fi
    bash "$main_script"
    exit $?
}

install_release() {
    rpm_installer="$(mktemp "${TMPDIR:-/tmp}/ca-install-rpm.XXXXXX")"
    release_url="https://github.com/inclavare-containers/confidential-agent/releases/download/v$RELEASE_VERSION"
    curl -fsSL --retry 2 --connect-timeout 10 --max-time 90 -o "$rpm_installer" "$release_url/install-rpm.sh"
    if [ "$(head -n 1 "$rpm_installer")" != '#!/usr/bin/env bash' ]; then
        echo "Invalid RPM installer downloaded from $release_url/install-rpm.sh" >&2
        exit 1
    fi
    bash "$rpm_installer" --version "$RELEASE_VERSION"
}

ensure_git() {
    if command -v git >/dev/null 2>&1; then
        return 0
    fi
    if [ "$(id -u)" != "0" ]; then
        echo "git is required for source workflows. Re-run as root or install git first." >&2
        exit 2
    fi
    if command -v dnf >/dev/null 2>&1; then
        dnf install -y git ca-certificates
    elif command -v yum >/dev/null 2>&1; then
        yum install -y git ca-certificates
    else
        echo "git is required and no yum/dnf package manager was found." >&2
        exit 2
    fi
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

checkout_source() {
    if [ ! -e "$source_dir/.git" ]; then
        mkdir -p "$source_dir"
        git -C "$source_dir" init
        git -C "$source_dir" remote add origin "$repo"
    fi
    git_fetch_with_fallback "$source_dir" "$repo" "$ref"
    git -C "$source_dir" checkout --detach FETCH_HEAD
}

launch_github_frontend() {
    slug="${1%.git}"
    raw="https://raw.githubusercontent.com/$slug/$2"
    frontend_dir="$(mktemp -d "${TMPDIR:-/tmp}/ca-one-click-frontend.XXXXXX")"
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
    sh "$frontend_dir/bootstrap.sh" "$first" "$second" install.sh "$pass_args_file"
}

bootstrap_runtime() {
    case "$CA_BOOTSTRAP_ACTION" in
        release)
            rpm_installer=""
            trap 'if [ -n "$rpm_installer" ]; then rm -f "$rpm_installer"; fi' EXIT
            install_release
            ;;
        source)
            repo="$CA_BOOTSTRAP_REPO"
            ref="$CA_BOOTSTRAP_REF"
            source_dir="$CA_BOOTSTRAP_ROOT"
            ensure_git
            checkout_source
            ;;
    esac
}

# Wrap the imperative body so `sh` parses the entire script before executing
# anything. Without this wrapper, `curl ... | sh` would hit `exec </dev/tty`
# while the rest of the script bytes are still in the curl pipe, then sh would
# try to read the remaining script from the terminal and silently hang.
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
                explicit_ref=1
                commit_ref=1
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

    if [ -z "$explicit_ref" ] && [ -n "$source_dir" ] && [ -f "$source_dir/one-click/lib/main.sh" ]; then
        run_main "$source_dir/one-click/lib/main.sh"
    fi

    if [ -z "$explicit_ref" ] && [ -z "$source_dir" ]; then
        if [ -f "$0" ]; then
            script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
            if [ -f "$script_dir/../Cargo.toml" ] && [ -f "$script_dir/lib/main.sh" ]; then
                run_main "$script_dir/lib/main.sh"
            fi
            if [ "$script_dir" = "$PACKAGED_DIR" ]; then
                export CA_PACKAGED_INSTALL=1
                run_main "$PACKAGED_DIR/lib/main.sh"
            fi
        fi

        export CA_PACKAGED_INSTALL=1 CA_RELEASE_VERSION="$RELEASE_VERSION"
        export CA_BOOTSTRAP_ROOT="${PACKAGED_DIR%/one-click}" CA_BOOTSTRAP_ACTION=release
        launch_github_frontend "${DEFAULT_REPO#https://github.com/}" "v$RELEASE_VERSION"
        return
    fi

    source_dir="$(realpath -m -- "${source_dir:-${HOME:-/root}/.cache/confidential-agent/source}")"
    case "$repo" in
        https://github.com/*)
            export CA_PACKAGED_INSTALL=0 CA_BOOTSTRAP_ROOT="$source_dir" CA_BOOTSTRAP_ACTION=source
            export CA_BOOTSTRAP_REPO="$repo" CA_BOOTSTRAP_REF="$ref"
            launch_github_frontend "${repo#https://github.com/}" "$ref"
            ;;
        *)
            checkout_source
            run_main "$source_dir/one-click/lib/main.sh"
            ;;
    esac
}

main "$@"
