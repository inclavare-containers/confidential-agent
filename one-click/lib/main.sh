#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=one-click/lib/common.sh
. "$SCRIPT_DIR/common.sh"
. "$ROOT_DIR/common/software-versions.sh"
export CA_SOFTWARE_VERSIONS_FILE="$ROOT_DIR/software-versions.json"
# shellcheck source=one-click/lib/deps-alinux.sh
. "$SCRIPT_DIR/deps-alinux.sh"
# shellcheck source=one-click/lib/credentials.sh
. "$SCRIPT_DIR/credentials.sh"
# shellcheck source=one-click/lib/openclaw.sh
. "$SCRIPT_DIR/openclaw.sh"
. "$SCRIPT_DIR/harness.sh"
# shellcheck source=one-click/lib/deploy.sh
. "$SCRIPT_DIR/deploy.sh"
# shellcheck source=one-click/lib/cpu-state.sh
. "$SCRIPT_DIR/cpu-state.sh"
ROOT_DIR="${CA_BOOTSTRAP_ROOT:-$ROOT_DIR}"

usage() {
  if [[ "${CA_ONE_CLICK_TARGET:-openclaw}" == "openclaw-vllm" ]]; then
    cat <<EOF
Usage:
  one-click/install-openclaw-vllm.sh [deploy|install-only|cleanup] [options]

Default mode:
  deploy

Common options:
  --non-interactive              Do not prompt; fail if required values are missing
  --yes                          Assume yes for safe replacement prompts
  --state-dir PATH               Confidential Agent state directory
  --work-dir PATH                One-click generated files/log directory
  --tools-dir PATH               Native tools directory
  --skip-deps                    Skip OS package and one-click Python installation
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --install-host-client          Install the local OpenClaw CLI and its prerequisites
  --skip-host-client             Skip the local CLI installation and prompt
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --help                         Show this help

Deploy options:
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-l
  --instance-type TYPE           Default: ecs.gn8v-tee.4xlarge
  --disk-gb GB                   Default: 512
  --allowed-cidr CIDR[,CIDR...]  Operator access CIDR list; may be repeated
  --gateway-token TOKEN          OpenClaw gateway token; generated if omitted
  --openclaw-version VERSION     Default: 2026.10.3
  --node-version VERSION         Default: 24.21.0
  --npm-registry URL             npm registry used inside the guest image
  --vllm-model-id ID             vLLM ModelScope model id; default: Qwen/Qwen3.6-35B-A3B
  --vllm-model-dir PATH          vLLM model directory; default: /opt/models/Qwen3.6-35B-A3B
  --vllm-served-model-name NAME  vLLM served model name; default: Qwen3.6-35B-A3B
  --vllm-port PORT               vLLM OpenAI-compatible API port; default: 8090
  --vllm-version VERSION         vLLM package version; default: 0.28.0
  --vllm-build-variants LIST     vLLM build variants: release, debug, or release,debug; default: release
  --enable-dingtalk              Enable DingTalk channel
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when missing or incompatible

Examples:
  one-click/install-openclaw-vllm.sh
  one-click/install-openclaw-vllm.sh --vllm-build-variants debug
  one-click/install-openclaw-vllm.sh cleanup --state-dir "\$HOME/.confidential-agent"
EOF
    return
  fi

  if [[ "${CA_ONE_CLICK_TARGET:-openclaw}" == "vllm-inference" ]]; then
    cat <<EOF
Usage:
  one-click/install-vllm-inference.sh [deploy|install-only|cleanup] [options]

Default mode:
  deploy

Common options:
  --non-interactive              Do not prompt; fail if required values are missing
  --yes                          Assume yes for safe replacement prompts
  --state-dir PATH               Confidential Agent state directory
  --work-dir PATH                One-click generated files/log directory
  --tools-dir PATH               Native tools directory
  --skip-deps                    Skip OS package and one-click Python installation
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --help                         Show this help

Deploy options:
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-l
  --instance-type TYPE           Default: ecs.gn8v-tee.4xlarge
  --disk-gb GB                   Default: 512
  --replicas N                   Default: 1 (single-node); N>1 = N replicas behind an ALB with OHTTP-encrypted connect
  --allowed-cidr CIDR[,CIDR...]  Operator access CIDR list; may be repeated
  --gateway-token TOKEN          Connect tunnel token; generated if omitted
  --vllm-port PORT               vLLM OpenAI-compatible API port; default: 8080
  --vllm-version VERSION         vLLM package version; default: 0.28.0
  --vllm-build-variants LIST     vLLM image variants: release or debug; default: release
  --vllm-served-model-name NAME  vLLM served model name; default: Qwen3.8-27B
  --oss-bucket NAME              OSS bucket holding the encrypted model artifact (required)
  --oss-endpoint URL             Model download endpoint; default: https://oss-cn-beijing-internal.aliyuncs.com
  --image-spec PATH              Deploy an existing digest-pinned image AppSpec (JSON), without rebuilding
  --model-key-file PATH          Local gocryptfs model key file (required, must exist)
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when no binary exists

Examples:
  one-click/install-vllm-inference.sh --oss-bucket my-bucket --model-key-file /path/key
  one-click/install-vllm-inference.sh --vllm-served-model-name Qwen3.8-27B
  one-click/install-vllm-inference.sh cleanup --state-dir "\$HOME/.confidential-agent"
EOF
    return
  fi

  cat <<EOF
Usage:
  one-click/install.sh [deploy|continue|install-only|cleanup] [options]

Default mode:
  deploy

Common options:
  --non-interactive              Do not prompt; fail if required values are missing
  --yes                          Assume yes for safe replacement prompts
  --state-dir PATH               Confidential Agent state directory
  --work-dir PATH                One-click generated files/log directory
  --tools-dir PATH               Native tools directory
  --skip-deps                    Skip OS package and one-click Python installation
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --install-host-client          Install the client for the selected access method
  --skip-host-client             Skip the local CLI installation and prompt
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --disable-pep                  Do not install or enable the OpenClaw cai-pep policy service
  --help                         Show this help

CPU shared image options:
  --image-source MODE            prebuilt (default) or build
  --artifact PATH                Local Shelter artifact directory or manifest
  --verify-disk                  Also inspect the local prebuilt disk using Linux disk tools
  --image-release PATH           Explicit local image-release.json (requires --release-digest)
  --release-digest sha256:HEX    Trusted SHA-256 digest of --image-release
  --image-id ID                  Existing regional cloud ECS image ID (skips pull and publish)

Deploy options:
  --service-id NAME              Instance service ID (default: <harness>, i.e. openclaw)
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-i
  --instance-type TYPE           Default: ecs.g9i.xlarge
  --disk-gb GB                   Default: 200
  --replicas N                   Default: 1
  --allowed-cidr CIDR[,CIDR...]  Operator access CIDR list; may be repeated
  --harness NAME                 openclaw (default), hermes, codex, claude-code, qodercli, opencode, pi, dsh
  --access METHOD                web+tui, web, tui, http, ws, web+attach, attach, or ssh
  --harness-mode MODE            gateway, app-server, web, or cli
  --api-base-url URL             Inference API base URL; defaults to Bailian
  --api-key KEY                  Inference API key (or CA_API_KEY)
  --model MODEL                  Model id; default: $(ca_software_version bailian_model)
  --bailian-api-key KEY          Alias for --api-key
  --bailian-model MODEL          Alias for --model
  --gateway-token TOKEN          OpenClaw gateway token; generated if omitted
  --openclaw-version VERSION     Default: $(ca_software_version openclaw)
  --node-version VERSION         Default: $(ca_software_version node)
  --npm-registry URL             npm registry used for host clients and image builds
  --enable-dingtalk              Enable DingTalk channel (OpenClaw)
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor image build
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --skip-build                   Skip confidential image build (when --image-source build)
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when missing or incompatible

Examples:
  one-click/install.sh
  one-click/install.sh --non-interactive --yes --bailian-model qwen3.7-max
  one-click/install.sh --harness codex --access ws
  one-click/install.sh --image-source build --harness openclaw
  one-click/install.sh continue --service-id openclaw
  one-click/install.sh install-only
  one-click/install.sh cleanup --state-dir "\$HOME/.confidential-agent"
EOF
}

init_defaults() {
  CA_ONE_CLICK_TARGET="${CA_ONE_CLICK_TARGET:-openclaw}"
  case "$CA_ONE_CLICK_TARGET" in
    openclaw) CA_MODE="deploy-openclaw" ;;
    openclaw-vllm) CA_MODE="deploy-openclaw-vllm" ;;
    vllm-inference) CA_MODE="deploy-vllm-inference" ;;
    *) die "unsupported CA_ONE_CLICK_TARGET: $CA_ONE_CLICK_TARGET" ;;
  esac
  if [[ -z "${CA_PACKAGED_INSTALL:-}" ]]; then
    if [[ ! -f "$ROOT_DIR/Cargo.toml" ]]; then
      CA_PACKAGED_INSTALL=1
    else
      CA_PACKAGED_INSTALL=0
    fi
  fi
  CA_NON_INTERACTIVE="${CA_NON_INTERACTIVE:-0}"
  CA_ASSUME_YES="${CA_ASSUME_YES:-0}"
  CA_SKIP_DEPS="${CA_SKIP_DEPS:-0}"
  CA_SKIP_RUSTUP="${CA_SKIP_RUSTUP:-1}"
  CA_SKIP_CARGO_BUILD="${CA_SKIP_CARGO_BUILD:-0}"
  CA_SKIP_BUILD="${CA_SKIP_BUILD:-0}"
  CA_IMAGE_SPEC="${CA_IMAGE_SPEC:-}"
  CA_SKIP_DEPLOY="${CA_SKIP_DEPLOY:-0}"
  CA_SKIP_HOST_OPENCLAW="${CA_SKIP_HOST_OPENCLAW:-0}"
  CA_SKIP_HOST_CLIENT="${CA_SKIP_HOST_CLIENT:-0}"
  CA_INSTALL_HOST_CLIENT="${CA_INSTALL_HOST_CLIENT:-0}"
  CA_HOST_CLIENT=""
  CA_ENABLE_DINGTALK="${CA_ENABLE_DINGTALK:-0}"
  CA_DISABLE_PEP="${CA_DISABLE_PEP:-0}"
  CA_START_CONNECT="${CA_START_CONNECT:-1}"
  CA_REGION="${CA_REGION:-cn-beijing}"
  CA_ZONE_ID="${CA_ZONE_ID:-}"
  CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-}"
  CA_DISK_GB="${CA_DISK_GB:-}"
  CA_REPLICAS="${CA_REPLICAS:-1}"
  CA_REFERENCE_VALUES="${CA_REFERENCE_VALUES:-rekor}"
  CA_SERVICE_ID_OVERRIDE=""
  CA_HARNESS="${CA_HARNESS:-}"
  CA_HARNESS_MODE="${CA_HARNESS_MODE:-}"
  CA_ACCESS_METHOD="${CA_ACCESS_METHOD:-}"
  CA_MODEL="${CA_MODEL:-${CA_BAILIAN_MODEL:-${DASHSCOPE_MODEL:-}}}"
  CA_API_KEY="${CA_API_KEY:-${CA_BAILIAN_API_KEY:-}}"
  CA_API_BASE_URL="${CA_API_BASE_URL:-}"
  CA_IMAGE_SOURCE="${CA_IMAGE_SOURCE:-}"
  CA_ARTIFACT="${CA_ARTIFACT:-}"
  CA_VERIFY_DISK="${CA_VERIFY_DISK:-0}"
  CA_IMAGE_ID="${CA_IMAGE_ID:-}"
  CA_IMAGE_RELEASE="${CA_IMAGE_RELEASE:-}"
  CA_RELEASE_DIGEST="${CA_RELEASE_DIGEST:-}"
  CA_OPENCLAW_VERSION="${CA_OPENCLAW_VERSION:-$(ca_software_version openclaw)}"
  CA_NODE_VERSION="${CA_NODE_VERSION:-$(ca_software_version node)}"
  CA_NPM_REGISTRY="${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  CA_OPENCLAW_VLLM_MODEL_ID="${CA_OPENCLAW_VLLM_MODEL_ID:-${OPENCLAW_VLLM_MODEL_ID:-Qwen/Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_MODEL_DIR="${CA_OPENCLAW_VLLM_MODEL_DIR:-${OPENCLAW_VLLM_MODEL_DIR:-/opt/models/Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_SERVED_MODEL_NAME="${CA_OPENCLAW_VLLM_SERVED_MODEL_NAME:-${OPENCLAW_VLLM_SERVED_MODEL_NAME:-Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_PORT="${CA_OPENCLAW_VLLM_PORT:-${OPENCLAW_VLLM_PORT:-8090}}"
  CA_OPENCLAW_VLLM_VERSION="${CA_OPENCLAW_VLLM_VERSION:-${OPENCLAW_VLLM_VERSION:-0.28.0}}"
  CA_OPENCLAW_VLLM_BUILD_VARIANTS="${CA_OPENCLAW_VLLM_BUILD_VARIANTS:-${OPENCLAW_VLLM_BUILD_VARIANTS:-release}}"
  CA_VLLM_INFERENCE_PORT="${CA_VLLM_INFERENCE_PORT:-${VLLM_INFERENCE_PORT:-8080}}"
  CA_VLLM_INFERENCE_VERSION="${CA_VLLM_INFERENCE_VERSION:-${VLLM_INFERENCE_VERSION:-0.28.0}}"
  CA_VLLM_INFERENCE_BUILD_VARIANTS="${CA_VLLM_INFERENCE_BUILD_VARIANTS:-${VLLM_INFERENCE_BUILD_VARIANTS:-release}}"
  CA_VLLM_INFERENCE_SERVED_MODEL_NAME="${CA_VLLM_INFERENCE_SERVED_MODEL_NAME:-${VLLM_INFERENCE_SERVED_MODEL_NAME:-Qwen3.8-27B}}"
  CA_OSS_BUCKET="${CA_OSS_BUCKET:-${OSS_BUCKET:-}}"
  CA_OSS_ENDPOINT="${CA_OSS_ENDPOINT:-${OSS_ENDPOINT:-https://oss-cn-beijing-internal.aliyuncs.com}}"
  CA_MODEL_KEY_FILE="${CA_MODEL_KEY_FILE:-${MODEL_KEY_FILE:-}}"
  CA_TOOLS_DIR="${CA_TOOLS_DIR:-/usr/libexec/cagent/bin}"
  CA_STATE_DIR="${CA_STATE_DIR:-${HOME:-/root}/.confidential-agent}"
  CA_WORK_DIR_EXPLICIT=0
  [[ -z "${CA_WORK_DIR:-}" ]] || CA_WORK_DIR_EXPLICIT=1
  CA_WORK_DIR="${CA_WORK_DIR:-$CA_STATE_DIR/one-click}"
  CA_SLSA_GENERATOR="${CA_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
  CA_STATUS_TIMEOUT_SEC="${CA_STATUS_TIMEOUT_SEC:-900}"
  CA_CONNECT_TIMEOUT_SEC="${CA_CONNECT_TIMEOUT_SEC:-240}"
  CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-}"
  CA_READY_JSON="${CA_READY_JSON:-}"
  if [[ "$CA_PACKAGED_INSTALL" == 1 ]]; then
    CA_BIN="${CA_BIN:-/usr/bin/confidential-agent}"
  else
    CA_BIN="${CA_BIN:-$ROOT_DIR/target/release/confidential-agent}"
  fi
  CA_AGENTD_BIN="${CA_AGENTD_BIN:-$(dirname "$CA_BIN")/confidential-agentd}"
  CA_GATEWAY_BIN="${CA_GATEWAY_BIN:-$(dirname "$CA_BIN")/cai-gateway}"
  CA_PEP_BIN="${CA_PEP_BIN:-$(dirname "$CA_BIN")/cai-pep}"
}

parse_args() {
  if (($# > 0)); then
    case "$1" in
      deploy|deploy-openclaw|deploy-openclaw-vllm|deploy-vllm-inference|continue|install-only|cleanup|cleanup-openclaw-vllm|cleanup-vllm-inference)
        normalize_mode "$1"
        shift
        ;;
    esac
  fi

  while (($# > 0)); do
    if [[ "$CA_MODE" == continue ]]; then
      case "$1" in
        --state-dir|--service-id|--non-interactive|--help|-h) ;;
        *) die "continue reuses the original inputs; unsupported option: $1" ;;
      esac
    fi
    case "$1" in
      --non-interactive) CA_NON_INTERACTIVE=1; shift ;;
      --yes|-y) CA_ASSUME_YES=1; shift ;;
      --state-dir) CA_STATE_DIR="${2:?missing value for --state-dir}"; shift 2 ;;
      --work-dir) CA_WORK_DIR="${2:?missing value for --work-dir}"; CA_WORK_DIR_EXPLICIT=1; shift 2 ;;
      --tools-dir) CA_TOOLS_DIR="${2:?missing value for --tools-dir}"; shift 2 ;;
      --skip-deps) CA_SKIP_DEPS=1; shift ;;
      --skip-rustup) CA_SKIP_RUSTUP=1; shift ;;
      --allow-rustup) CA_SKIP_RUSTUP=0; shift ;;
      --skip-cargo-build) CA_SKIP_CARGO_BUILD=1; shift ;;
      --install-host-client) CA_INSTALL_HOST_CLIENT=1; shift ;;
      --skip-host-client) CA_SKIP_HOST_CLIENT=1; shift ;;
      --skip-host-openclaw) CA_SKIP_HOST_OPENCLAW=1; shift ;;
      --disable-pep) CA_DISABLE_PEP=1; shift ;;
      --service-id) CA_SERVICE_ID_OVERRIDE="${2:?missing value for --service-id}"; shift 2 ;;
      --region) CA_REGION="${2:?missing value for --region}"; shift 2 ;;
      --zone-id) CA_ZONE_ID="${2:?missing value for --zone-id}"; shift 2 ;;
      --instance-type) CA_INSTANCE_TYPE="${2:?missing value for --instance-type}"; shift 2 ;;
      --disk-gb) CA_DISK_GB="${2:?missing value for --disk-gb}"; shift 2 ;;
      --replicas) CA_REPLICAS="${2:?missing value for --replicas}"; shift 2 ;;
      --allowed-cidr)
        if [[ -n "${CA_ALLOWED_CIDR:-}" ]]; then
          CA_ALLOWED_CIDR="$CA_ALLOWED_CIDR,${2:?missing value for --allowed-cidr}"
        else
          CA_ALLOWED_CIDR="${2:?missing value for --allowed-cidr}"
        fi
        shift 2
        ;;
      --harness) CA_HARNESS="${2:?missing value for --harness}"; shift 2 ;;
      --access) CA_ACCESS_METHOD="${2:?missing value for $1}"; shift 2 ;;
      --harness-mode) CA_HARNESS_MODE="${2:?missing value for --harness-mode}"; shift 2 ;;
      --api-key|--bailian-api-key) CA_API_KEY="${2:?missing API key}"; shift 2 ;;
      --api-base-url) CA_API_BASE_URL="${2:?missing value for --api-base-url}"; shift 2 ;;
      --model|--bailian-model) CA_MODEL="${2:?missing model}"; shift 2 ;;
      --image-source) CA_IMAGE_SOURCE="${2:?missing value for --image-source}"; shift 2 ;;
      --artifact) CA_ARTIFACT="${2:?missing value for --artifact}"; shift 2 ;;
      --verify-disk) CA_VERIFY_DISK=1; shift ;;
      --image-id) CA_IMAGE_ID="${2:?missing value for --image-id}"; shift 2 ;;
      --image-release) CA_IMAGE_RELEASE="${2:?missing value for --image-release}"; shift 2 ;;
      --release-digest) CA_RELEASE_DIGEST="${2:?missing value for --release-digest}"; shift 2 ;;
      --gateway-token) CA_GATEWAY_TOKEN="${2:?missing value for --gateway-token}"; shift 2 ;;
      --openclaw-version) CA_OPENCLAW_VERSION="${2:?missing value for --openclaw-version}"; shift 2 ;;
      --node-version) CA_NODE_VERSION="${2:?missing value for --node-version}"; shift 2 ;;
      --npm-registry) CA_NPM_REGISTRY="${2:?missing value for --npm-registry}"; shift 2 ;;
      --vllm-model-id) CA_OPENCLAW_VLLM_MODEL_ID="${2:?missing value for --vllm-model-id}"; shift 2 ;;
      --vllm-model-dir) CA_OPENCLAW_VLLM_MODEL_DIR="${2:?missing value for --vllm-model-dir}"; shift 2 ;;
      --vllm-served-model-name)
        if [[ "$CA_ONE_CLICK_TARGET" == "vllm-inference" ]]; then
          CA_VLLM_INFERENCE_SERVED_MODEL_NAME="${2:?missing value for --vllm-served-model-name}"
        else
          CA_OPENCLAW_VLLM_SERVED_MODEL_NAME="${2:?missing value for --vllm-served-model-name}"
        fi
        shift 2
        ;;
      --vllm-port)
        if [[ "$CA_ONE_CLICK_TARGET" == "vllm-inference" ]]; then
          CA_VLLM_INFERENCE_PORT="${2:?missing value for --vllm-port}"
        else
          CA_OPENCLAW_VLLM_PORT="${2:?missing value for --vllm-port}"
        fi
        shift 2
        ;;
      --vllm-version)
        if [[ "$CA_ONE_CLICK_TARGET" == "vllm-inference" ]]; then
          CA_VLLM_INFERENCE_VERSION="${2:?missing value for --vllm-version}"
        else
          CA_OPENCLAW_VLLM_VERSION="${2:?missing value for --vllm-version}"
        fi
        shift 2
        ;;
      --vllm-build-variants)
        if [[ "$CA_ONE_CLICK_TARGET" == "vllm-inference" ]]; then
          CA_VLLM_INFERENCE_BUILD_VARIANTS="${2:?missing value for --vllm-build-variants}"
        else
          CA_OPENCLAW_VLLM_BUILD_VARIANTS="${2:?missing value for --vllm-build-variants}"
        fi
        shift 2
        ;;
      --oss-bucket) CA_OSS_BUCKET="${2:?missing value for --oss-bucket}"; shift 2 ;;
      --oss-endpoint) CA_OSS_ENDPOINT="${2:?missing value for --oss-endpoint}"; shift 2 ;;
      --model-key-file) CA_MODEL_KEY_FILE="${2:?missing value for --model-key-file}"; shift 2 ;;
      --enable-dingtalk) CA_ENABLE_DINGTALK=1; shift ;;
      --reference-values) CA_REFERENCE_VALUES="${2:?missing value for --reference-values}"; shift 2 ;;
      --cosign-key) CA_COSIGN_KEY="${2:?missing value for --cosign-key}"; shift 2 ;;
      --slsa-generator) CA_SLSA_GENERATOR="${2:?missing value for --slsa-generator}"; shift 2 ;;
      --skip-build) CA_SKIP_BUILD=1; shift ;;
      --image-spec) CA_IMAGE_SPEC="${2:?missing value for --image-spec}"; shift 2 ;;
      --skip-deploy) CA_SKIP_DEPLOY=1; shift ;;
      --no-start-connect) CA_START_CONNECT=0; shift ;;
      --shelter-bin) CA_SHELTER_BIN="${2:?missing value for --shelter-bin}"; shift 2 ;;
      --shelter-rpm) CA_SHELTER_RPM="${2:?missing value for --shelter-rpm}"; shift 2 ;;
      --help|-h) usage; exit 0 ;;
      *) die "unknown option or mode: $1" ;;
    esac
  done
}

normalize_mode() {
  local requested="$1"
  case "$CA_ONE_CLICK_TARGET:$requested" in
    openclaw:deploy|openclaw:deploy-openclaw) CA_MODE="deploy-openclaw" ;;
    openclaw:continue) CA_MODE="continue" ;;
    openclaw:cleanup) CA_MODE="cleanup" ;;
    openclaw:install-only) CA_MODE="install-only" ;;
    openclaw:deploy-openclaw-vllm|openclaw:cleanup-openclaw-vllm)
      die "use one-click/install-openclaw-vllm.sh for OpenClaw vLLM"
      ;;
    openclaw-vllm:deploy|openclaw-vllm:deploy-openclaw-vllm) CA_MODE="deploy-openclaw-vllm" ;;
    openclaw-vllm:cleanup|openclaw-vllm:cleanup-openclaw-vllm) CA_MODE="cleanup-openclaw-vllm" ;;
    openclaw-vllm:install-only) CA_MODE="install-only" ;;
    openclaw-vllm:deploy-openclaw)
      die "use one-click/install.sh for OpenClaw with Bailian API"
      ;;
    vllm-inference:deploy|vllm-inference:deploy-vllm-inference) CA_MODE="deploy-vllm-inference" ;;
    vllm-inference:cleanup) CA_MODE="cleanup-vllm-inference" ;;
    vllm-inference:install-only) CA_MODE="install-only" ;;
    *) die "unsupported mode for $CA_ONE_CLICK_TARGET: $requested" ;;
  esac
}

apply_target_defaults() {
  local target="$CA_ONE_CLICK_TARGET"
  case "$CA_MODE" in
    deploy-openclaw|cleanup) target="openclaw" ;;
    deploy-openclaw-vllm|cleanup-openclaw-vllm) target="openclaw-vllm" ;;
    deploy-vllm-inference|cleanup-vllm-inference) target="vllm-inference" ;;
    install-only) ;;
    *) die "unsupported mode: $CA_MODE" ;;
  esac

  case "$target" in
    openclaw)
      local harness="${CA_HARNESS:-openclaw}"
      if [[ -n "${CA_SERVICE_ID_OVERRIDE:-}" ]]; then
        CA_SERVICE_ID="$CA_SERVICE_ID_OVERRIDE"
      else
        CA_SERVICE_ID="$harness"
      fi
      if [[ "$harness" == openclaw && "$CA_SERVICE_ID" == openclaw ]]; then
        CA_SERVICE_LABEL="OpenClaw"
      else
        CA_SERVICE_LABEL="$harness"
      fi
      CA_PROJECT_NAME="$CA_SERVICE_ID"
      CA_SPEC_NAME="instance.yaml"
      if [[ "$CA_SERVICE_ID" == "openclaw" ]]; then
        CA_CONNECT_NAME="connect"
      else
        CA_CONNECT_NAME="connect-$CA_SERVICE_ID"
      fi
      CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-i}"
      CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.g9i.xlarge}"
      CA_DISK_GB="${CA_DISK_GB:-120}"
      if [[ -z "${CA_CONNECT_READINESS_PATH:-}" ]]; then
        case "$harness" in
          openclaw) CA_CONNECT_READINESS_PATH="/openclaw" ;;
        esac
      fi
      ;;
    openclaw-vllm)
      CA_SERVICE_ID="${CA_SERVICE_ID_OVERRIDE:-openclaw-vllm}"
      CA_SERVICE_LABEL="OpenClaw vLLM"
      CA_PROJECT_NAME="$CA_SERVICE_ID"
      CA_SPEC_NAME="openclaw-vllm.yaml"
      CA_CONNECT_NAME="connect-openclaw-vllm"
      CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-l}"
      CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}"
      CA_DISK_GB="${CA_DISK_GB:-512}"
      CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-/openclaw}"
      ;;
    vllm-inference)
      CA_SERVICE_ID="${CA_SERVICE_ID_OVERRIDE:-vllm-inference}"
      CA_SERVICE_LABEL="Secure LLM Inference"
      CA_PROJECT_NAME="$CA_SERVICE_ID"
      CA_SPEC_NAME="vllm-inference.yaml"
      CA_CONNECT_NAME="connect-vllm-inference"
      CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-l}"
      CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}"
      CA_DISK_GB="${CA_DISK_GB:-512}"
      CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-/health}"
      ;;
    *)
      die "unsupported mode: $CA_MODE"
      ;;
  esac
  CA_PROJECT_DIR="$CA_WORK_DIR/$CA_PROJECT_NAME"
  CA_SPEC_PATH="$CA_PROJECT_DIR/$CA_SPEC_NAME"
  CA_READY_JSON="${CA_READY_JSON:-$CA_WORK_DIR/$CA_CONNECT_NAME.json}"
}

validate_options() {
  [[ "$CA_SERVICE_ID" =~ ^[A-Za-z0-9_.-]+$ && "$CA_SERVICE_ID" != . && "$CA_SERVICE_ID" != .. ]] || die "invalid service ID: $CA_SERVICE_ID"
  if [[ "$CA_MODE" == deploy-openclaw || "$CA_MODE" == cleanup ]]; then
    [[ "$CA_SERVICE_ID" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid service ID: $CA_SERVICE_ID"
  fi
  if [[ "$CA_MODE" == deploy-openclaw || "$CA_MODE" == cleanup ]]; then
    is_supported_harness "$CA_HARNESS" || die "unsupported harness: $CA_HARNESS"
  fi
  if [[ "$CA_MODE" == deploy-openclaw ]]; then
    case "$CA_IMAGE_SOURCE" in
      prebuilt|build) ;;
      *) die "--image-source must be prebuilt or build" ;;
    esac
    if [[ "$CA_IMAGE_SOURCE" == "build" ]]; then
      [[ "$CA_VERIFY_DISK" != 1 ]] || die "--verify-disk applies to prebuilt artifacts"
      if [[ -n "$CA_ARTIFACT" || -n "$CA_IMAGE_RELEASE" || -n "$CA_RELEASE_DIGEST" || -n "$CA_IMAGE_ID" ]]; then
        die "--image-source build cannot be combined with --artifact, --image-release, --release-digest, or --image-id"
      fi
    else
      if [[ -n "$CA_IMAGE_RELEASE" && -z "$CA_RELEASE_DIGEST" ]]; then die "--image-release requires --release-digest"; fi
      if [[ -n "$CA_IMAGE_ID" && -n "$CA_ARTIFACT" ]]; then die "choose --image-id or --artifact"; fi
    fi
    if [[ "$CA_HARNESS" != openclaw ]]; then
      [[ "$CA_DISABLE_PEP" != 1 ]] || die "--disable-pep applies only to OpenClaw; the shared image requires cai-pep for local attestation"
      [[ "$CA_ENABLE_DINGTALK" != 1 ]] || die "--enable-dingtalk applies only to OpenClaw"
    fi
    [[ "$CA_REPLICAS" =~ ^[1-9][0-9]*$ ]] || die "--replicas must be a positive integer"
    if [[ "$CA_HARNESS_MODE" == "cli" && "$CA_REPLICAS" != "1" ]]; then
      die "CLI harnesses require --replicas 1; use separate service instances"
    fi
  elif [[ "$CA_MODE" == "deploy-openclaw-vllm" || "$CA_MODE" == "deploy-vllm-inference" ]]; then
    [[ "$CA_VERIFY_DISK" != 1 ]] || die "--verify-disk applies to the CPU prebuilt image"
    if [[ "$CA_IMAGE_SOURCE" != prebuilt || -n "$CA_ARTIFACT" || -n "$CA_IMAGE_RELEASE" || -n "$CA_RELEASE_DIGEST" || -n "$CA_IMAGE_ID" || -n "$CA_ACCESS_METHOD" ]]; then
      die "CPU shared-image options (--image-source/--artifact/--image-release/--release-digest/--image-id/--access) apply to one-click/install.sh only"
    fi
  fi
  if [[ -n "$CA_IMAGE_SPEC" ]]; then
    [[ "$CA_MODE" == deploy-vllm-inference ]] || die "--image-spec is supported by vllm-inference deploy only"
    [[ -f "$CA_IMAGE_SPEC" ]] || die "--image-spec must be an existing JSON AppSpec"
  fi
  case "$CA_REFERENCE_VALUES" in
    sample|rekor) ;;
    *) die "--reference-values must be sample or rekor" ;;
  esac
  [[ "$CA_DISK_GB" =~ ^[0-9]+$ ]] || die "--disk-gb must be an integer"
  if [[ "$CA_MODE" == "deploy-openclaw" ]] && ((CA_DISK_GB < 120)); then
    die "CPU shared image requires --disk-gb >= 120"
  fi
  [[ "$CA_OPENCLAW_VLLM_PORT" =~ ^[0-9]+$ ]] || die "--vllm-port must be an integer"
  # GPU OpenClaw direct specs unconditionally enable A2A, which requires Rekor metadata.
  if [[ "$CA_MODE" == deploy-openclaw-vllm && "$CA_REFERENCE_VALUES" != rekor ]]; then
    die "the OpenClaw vLLM flow enables A2A, which requires --reference-values rekor"
  fi
  if [[ "$CA_MODE" == "deploy-openclaw-vllm" ]]; then
    validate_openclaw_vllm_build_variants
  fi
  if [[ "$CA_MODE" == "deploy-openclaw-vllm" && "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    die "deploy-openclaw-vllm requires PEP because the TDX attestation skill uses cai-pep attest"
  fi
  if [[ "$CA_MODE" == "deploy-vllm-inference" ]]; then
    if [[ -z "$CA_IMAGE_SPEC" ]]; then
      [[ -n "$CA_OSS_BUCKET" ]] || die "--oss-bucket is required for vllm-inference"
      [[ -n "$CA_MODEL_KEY_FILE" && -f "$CA_MODEL_KEY_FILE" ]] || die "--model-key-file (existing file) is required for vllm-inference"
    fi
    [[ "$CA_VLLM_INFERENCE_PORT" =~ ^[0-9]+$ ]] || die "--vllm-port must be an integer"
  fi
}

validate_openclaw_vllm_build_variants() {
  local raw="${CA_OPENCLAW_VLLM_BUILD_VARIANTS//[[:space:]]/}"
  local token seen_release=0 seen_debug=0
  local -a tokens
  [[ -n "$raw" ]] || die "--vllm-build-variants cannot be empty"
  IFS=',' read -r -a tokens <<<"$raw"
  for token in "${tokens[@]}"; do
    case "$token" in
      release) seen_release=1 ;;
      debug) seen_debug=1 ;;
      "") die "--vllm-build-variants contains an empty entry" ;;
      *) die "--vllm-build-variants only supports release, debug, or release,debug" ;;
    esac
  done
  if [[ "$seen_release" != "1" && "$seen_debug" != "1" ]]; then
    die "--vllm-build-variants must enable at least one variant"
  fi
  if [[ "$seen_release" == "1" && "$seen_debug" == "1" ]]; then
    CA_OPENCLAW_VLLM_BUILD_VARIANTS="release,debug"
  elif [[ "$seen_release" == "1" ]]; then
    CA_OPENCLAW_VLLM_BUILD_VARIANTS="release"
  else
    CA_OPENCLAW_VLLM_BUILD_VARIANTS="debug"
  fi
}

normalize_workspace_paths() {
  CA_STATE_DIR="$(realpath -m -- "$CA_STATE_DIR")"
  if [[ "$CA_WORK_DIR_EXPLICIT" != "1" ]]; then CA_WORK_DIR="$CA_STATE_DIR/one-click"; fi
  CA_WORK_DIR="$(realpath -m -- "$CA_WORK_DIR")"
  CA_TOOLS_DIR="$(realpath -m -- "$CA_TOOLS_DIR")"
  export PATH="$CA_TOOLS_DIR:$PATH"
  if [[ "$CA_PACKAGED_INSTALL" == "1" ]]; then
    export CA_SHELTER_RPM_CACHE="${CA_SHELTER_RPM_CACHE:-$CA_STATE_DIR/cache/shelter}"
    export CA_RELEASE_IMAGE_CACHE="${CA_RELEASE_IMAGE_CACHE:-$CA_STATE_DIR/cache/release-image}"
  fi
  local variable
  for variable in CA_IMAGE_SPEC CA_MODEL_KEY_FILE CA_COSIGN_KEY CA_IMAGE_RELEASE CA_READY_JSON; do
    if [[ -n "${!variable:-}" ]]; then printf -v "$variable" '%s' "$(realpath -m -- "${!variable}")"; fi
  done
  if [[ -e "$CA_ARTIFACT" ]]; then CA_ARTIFACT="$(realpath -m -- "$CA_ARTIFACT")"; fi
}

print_startup_config() {
  log "mode: $CA_MODE"
  log "service: $CA_SERVICE_ID"
  log "source: $ROOT_DIR"
  log "state_dir: $CA_STATE_DIR"
  log "work_dir: $CA_WORK_DIR"
  log "region/zone: $CA_REGION / $CA_ZONE_ID"
  log "instance_type: $CA_INSTANCE_TYPE"
  case "$CA_MODE" in
    deploy-openclaw-vllm|cleanup-openclaw-vllm)
      log "vllm_model: $CA_OPENCLAW_VLLM_MODEL_ID"
      log "vllm_served_model: $CA_OPENCLAW_VLLM_SERVED_MODEL_NAME"
      log "vllm_build_variants: $CA_OPENCLAW_VLLM_BUILD_VARIANTS"
      log "pep service: $([[ "$CA_DISABLE_PEP" == "1" ]] && printf disabled || printf enabled)"
      ;;
    deploy-vllm-inference|cleanup-vllm-inference)
      log "vllm_served_model: $CA_VLLM_INFERENCE_SERVED_MODEL_NAME"
      log "vllm_build_variants: $CA_VLLM_INFERENCE_BUILD_VARIANTS"
      ;;
    deploy-openclaw|cleanup)
      log "harness: $CA_HARNESS${CA_HARNESS_MODE:+ ($CA_HARNESS_MODE, access: $CA_ACCESS_METHOD)}"
      if [[ "$CA_MODE" == "deploy-openclaw" ]]; then
        log "image_source: $CA_IMAGE_SOURCE"
        if [[ "$CA_HARNESS" != qodercli ]]; then
          log "model: $CA_MODEL"
          log "api_base_url: $CA_API_BASE_URL"
        fi
        if [[ "$CA_HARNESS" == openclaw ]]; then
          log "pep service: $([[ "$CA_DISABLE_PEP" == "1" ]] && printf disabled || printf enabled)"
        else
          log "pep: local attestation helper; policy service disabled"
        fi
      fi
      ;;
  esac
  log "reference_values: $CA_REFERENCE_VALUES"
}

prepare_one_click_source() {
  if [[ -n "${CA_BOOTSTRAP_HOOK:-}" ]]; then
    bash "$CA_BOOTSTRAP_HOOK"
    ROOT_DIR="$CA_BOOTSTRAP_ROOT"
  fi
}

collect_gpu_inputs() {
  ensure_aliyun_cli_region
  collect_aliyun_credentials
  if [[ "$CA_MODE" == deploy-openclaw-vllm ]]; then
    resolve_dingtalk_enablement
    ensure_dingtalk_credentials
  fi
  resolve_allowed_cidr
  prepare_operator_peering
}

main() {
  init_defaults
  parse_args "$@"
  if [[ "$CA_MODE" == continue ]]; then
    resume_cpu_task
  else
    if [[ "$CA_MODE" == deploy-openclaw && -n "$CA_SERVICE_ID_OVERRIDE" ]]; then
      [[ "$CA_SERVICE_ID_OVERRIDE" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid service ID: $CA_SERVICE_ID_OVERRIDE"
      CA_STATE_DIR="$(realpath -m -- "$CA_STATE_DIR")"
      CA_SERVICE_ID="$CA_SERVICE_ID_OVERRIDE" reject_pending_cpu_task
    fi
    resolve_harness_options
  fi
  normalize_workspace_paths
  apply_target_defaults
  if [[ "${CA_CPU_RESUMING:-0}" != 1 ]]; then
    validate_options
    if [[ "$CA_MODE" == deploy-openclaw ]]; then reject_pending_cpu_task; fi
    resolve_host_client_install
  fi
  case "$CA_MODE" in
    deploy-openclaw) collect_cpu_inputs ;;
    deploy-openclaw-vllm|deploy-vllm-inference) collect_gpu_inputs ;;
    cleanup-openclaw-vllm|cleanup-vllm-inference) collect_aliyun_credentials ;;
  esac
  install -d -m 0700 "$CA_STATE_DIR" "$CA_WORK_DIR"
  print_startup_config

  case "$CA_MODE" in
    deploy-openclaw) run_deploy_cpu ;;
    deploy-openclaw-vllm) prepare_one_click_source; run_deploy_openclaw_vllm ;;
    deploy-vllm-inference) prepare_one_click_source; run_deploy_vllm_inference ;;
    install-only) prepare_one_click_source; run_install_only ;;
    cleanup) run_cleanup_cpu ;;
    cleanup-openclaw-vllm) prepare_one_click_source; run_cleanup_openclaw_vllm ;;
    cleanup-vllm-inference) prepare_one_click_source; run_cleanup_vllm_inference ;;
    *) die "unsupported mode: $CA_MODE" ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
