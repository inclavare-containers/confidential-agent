#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=one-click/lib/common.sh
. "$SCRIPT_DIR/common.sh"
# shellcheck source=one-click/lib/deps-alinux.sh
. "$SCRIPT_DIR/deps-alinux.sh"
# shellcheck source=one-click/lib/credentials.sh
. "$SCRIPT_DIR/credentials.sh"
# shellcheck source=one-click/lib/openclaw.sh
. "$SCRIPT_DIR/openclaw.sh"
# shellcheck source=one-click/lib/deploy.sh
. "$SCRIPT_DIR/deploy.sh"

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
  --tools-image NAME             Docker tools image tag
  --skip-deps                    Do not install OS packages
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --rebuild-tools-image          Rebuild the tools image even if it exists
  --help                         Show this help

Deploy options:
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-l
  --instance-type TYPE           Default: ecs.gn8v-tee.4xlarge
  --disk-gb GB                   Default: 512
  --allowed-cidr CIDR[,CIDR...]  Operator access CIDR list; may be repeated
  --gateway-token TOKEN          OpenClaw gateway token; generated if omitted
  --openclaw-version VERSION     Default: 2026.5.7
  --node-version VERSION         Default: 22.19.0
  --npm-registry URL             npm registry used inside the guest image
  --vllm-model-id ID             vLLM ModelScope model id; default: Qwen/Qwen3.6-35B-A3B
  --vllm-model-dir PATH          vLLM model directory; default: /opt/models/Qwen3.6-35B-A3B
  --vllm-served-model-name NAME  vLLM served model name; default: Qwen3.6-35B-A3B
  --vllm-port PORT               vLLM OpenAI-compatible API port; default: 8090
  --vllm-version VERSION         vLLM package version; default: 0.19.1
  --vllm-build-variants LIST     vLLM build variants: release, debug, or release,debug; default: release
  --enable-dingtalk              Enable DingTalk channel
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --build-backend MODE           mkosi or base-image; default: mkosi
  --base-image PATH              Required when --build-backend base-image is used
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when no binary exists

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
  --tools-image NAME             Docker tools image tag
  --skip-deps                    Do not install OS packages
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --rebuild-tools-image          Rebuild the tools image even if it exists
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
  --vllm-served-model-name NAME  vLLM served model name; default: Qwen3-32B
  --oss-bucket NAME              OSS bucket holding the encrypted model artifact (required)
  --oss-endpoint URL             OSS endpoint; default: https://oss-cn-beijing-internal.aliyuncs.com
  --model-key-file PATH          Local gocryptfs model key file (required, must exist)
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --build-backend MODE           mkosi or base-image; default: mkosi
  --base-image PATH              Required when --build-backend base-image is used
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when no binary exists

Examples:
  one-click/install-vllm-inference.sh --oss-bucket my-bucket --model-key-file /path/key
  one-click/install-vllm-inference.sh --vllm-served-model-name Qwen3-32B
  one-click/install-vllm-inference.sh cleanup --state-dir "\$HOME/.confidential-agent"
EOF
    return
  fi

  cat <<EOF
Usage:
  one-click/install.sh [deploy|install-only|cleanup] [options]

Default mode:
  deploy

Common options:
  --non-interactive              Do not prompt; fail if required values are missing
  --yes                          Assume yes for safe replacement prompts
  --state-dir PATH               Confidential Agent state directory
  --work-dir PATH                One-click generated files/log directory
  --tools-image NAME             Docker tools image tag
  --skip-deps                    Do not install OS packages
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host
  --rebuild-tools-image          Rebuild the tools image even if it exists
  --disable-pep                  Do not install or enable the OpenClaw cai-pep runtime
  --help                         Show this help

Deploy options:
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-i
  --instance-type TYPE           Default: ecs.g9i.xlarge
  --disk-gb GB                   Default: 200
  --allowed-cidr CIDR[,CIDR...]  Operator access CIDR list; may be repeated
  --bailian-api-key KEY          Bailian/DashScope API key
  --bailian-model MODEL          Bailian model id; default: qwen3.7-max
  --gateway-token TOKEN          OpenClaw gateway token; generated if omitted
  --openclaw-version VERSION     Default: 2026.5.7
  --node-version VERSION         Default: 22.19.0
  --npm-registry URL             npm registry used inside the guest image
  --enable-dingtalk              Enable DingTalk channel
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --build-backend MODE           mkosi or base-image; default: mkosi
  --base-image PATH              Required when --build-backend base-image is used
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when no binary exists

Examples:
  one-click/install.sh
  one-click/install.sh install-only     # installs Confidential Agent CLI/Shelter/tools only
  one-click/install.sh --enable-dingtalk
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
  CA_NON_INTERACTIVE="${CA_NON_INTERACTIVE:-0}"
  CA_ASSUME_YES="${CA_ASSUME_YES:-0}"
  CA_SKIP_DEPS="${CA_SKIP_DEPS:-0}"
  CA_SKIP_RUSTUP="${CA_SKIP_RUSTUP:-1}"
  CA_SKIP_CARGO_BUILD="${CA_SKIP_CARGO_BUILD:-0}"
  CA_SKIP_BUILD="${CA_SKIP_BUILD:-0}"
  CA_SKIP_DEPLOY="${CA_SKIP_DEPLOY:-0}"
  CA_SKIP_HOST_OPENCLAW="${CA_SKIP_HOST_OPENCLAW:-0}"
  CA_REBUILD_TOOLS_IMAGE="${CA_REBUILD_TOOLS_IMAGE:-0}"
  CA_ENABLE_DINGTALK="${CA_ENABLE_DINGTALK:-0}"
  CA_DISABLE_PEP="${CA_DISABLE_PEP:-0}"
  CA_START_CONNECT="${CA_START_CONNECT:-1}"
  CA_REGION="${CA_REGION:-cn-beijing}"
  CA_ZONE_ID="${CA_ZONE_ID:-}"
  CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-}"
  CA_DISK_GB="${CA_DISK_GB:-}"
  CA_REPLICAS="${CA_REPLICAS:-1}"
  CA_REFERENCE_VALUES="${CA_REFERENCE_VALUES:-rekor}"
  CA_BAILIAN_MODEL="${CA_BAILIAN_MODEL:-qwen3.7-max}"
  CA_OPENCLAW_VERSION="${CA_OPENCLAW_VERSION:-2026.5.7}"
  CA_NODE_VERSION="${CA_NODE_VERSION:-22.19.0}"
  CA_NPM_REGISTRY="${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  CA_OPENCLAW_VLLM_MODEL_ID="${CA_OPENCLAW_VLLM_MODEL_ID:-${OPENCLAW_VLLM_MODEL_ID:-Qwen/Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_MODEL_DIR="${CA_OPENCLAW_VLLM_MODEL_DIR:-${OPENCLAW_VLLM_MODEL_DIR:-/opt/models/Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_SERVED_MODEL_NAME="${CA_OPENCLAW_VLLM_SERVED_MODEL_NAME:-${OPENCLAW_VLLM_SERVED_MODEL_NAME:-Qwen3.6-35B-A3B}}"
  CA_OPENCLAW_VLLM_PORT="${CA_OPENCLAW_VLLM_PORT:-${OPENCLAW_VLLM_PORT:-8090}}"
  CA_OPENCLAW_VLLM_VERSION="${CA_OPENCLAW_VLLM_VERSION:-${OPENCLAW_VLLM_VERSION:-0.19.1}}"
  CA_OPENCLAW_VLLM_BUILD_VARIANTS="${CA_OPENCLAW_VLLM_BUILD_VARIANTS:-${OPENCLAW_VLLM_BUILD_VARIANTS:-release}}"
  CA_VLLM_INFERENCE_PORT="${CA_VLLM_INFERENCE_PORT:-${VLLM_INFERENCE_PORT:-8080}}"
  CA_VLLM_INFERENCE_VERSION="${CA_VLLM_INFERENCE_VERSION:-${VLLM_INFERENCE_VERSION:-0.28.0}}"
  CA_VLLM_INFERENCE_BUILD_VARIANTS="${CA_VLLM_INFERENCE_BUILD_VARIANTS:-${VLLM_INFERENCE_BUILD_VARIANTS:-release}}"
  CA_VLLM_INFERENCE_SERVED_MODEL_NAME="${CA_VLLM_INFERENCE_SERVED_MODEL_NAME:-${VLLM_INFERENCE_SERVED_MODEL_NAME:-Qwen3-32B}}"
  CA_OSS_BUCKET="${CA_OSS_BUCKET:-${OSS_BUCKET:-}}"
  CA_OSS_ENDPOINT="${CA_OSS_ENDPOINT:-${OSS_ENDPOINT:-https://oss-cn-beijing-internal.aliyuncs.com}}"
  CA_MODEL_KEY_FILE="${CA_MODEL_KEY_FILE:-${MODEL_KEY_FILE:-}}"
  CA_BUILD_BACKEND="${CA_BUILD_BACKEND:-mkosi}"
  CA_BASE_IMAGE="${CA_BASE_IMAGE:-}"
  CA_TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
  CA_STATE_DIR="${CA_STATE_DIR:-${HOME:-/root}/.confidential-agent}"
  CA_WORK_DIR="${CA_WORK_DIR:-$CA_STATE_DIR/one-click}"
  CA_SLSA_GENERATOR="${CA_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
  CA_STATUS_TIMEOUT_SEC="${CA_STATUS_TIMEOUT_SEC:-900}"
  CA_CONNECT_TIMEOUT_SEC="${CA_CONNECT_TIMEOUT_SEC:-240}"
  CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-/openclaw}"
  CA_BIN="${CA_BIN:-$ROOT_DIR/target/release/confidential-agent}"
  CA_AGENTD_BIN="${CA_AGENTD_BIN:-$(dirname "$CA_BIN")/confidential-agentd}"
  CA_GATEWAY_BIN="${CA_GATEWAY_BIN:-$(dirname "$CA_BIN")/cai-gateway}"
  CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/release/cai-pep}"
}

parse_args() {
  if (($# > 0)); then
    case "$1" in
      deploy|deploy-openclaw|deploy-openclaw-vllm|deploy-vllm-inference|install-only|cleanup|cleanup-openclaw-vllm|cleanup-vllm-inference)
        normalize_mode "$1"
        shift
        ;;
    esac
  fi

  while (($# > 0)); do
    case "$1" in
      --non-interactive) CA_NON_INTERACTIVE=1; shift ;;
      --yes|-y) CA_ASSUME_YES=1; shift ;;
      --state-dir) CA_STATE_DIR="${2:?missing value for --state-dir}"; shift 2 ;;
      --work-dir) CA_WORK_DIR="${2:?missing value for --work-dir}"; shift 2 ;;
      --tools-image) CA_TOOLS_IMAGE="${2:?missing value for --tools-image}"; shift 2 ;;
      --skip-deps) CA_SKIP_DEPS=1; shift ;;
      --skip-rustup) CA_SKIP_RUSTUP=1; shift ;;
      --allow-rustup) CA_SKIP_RUSTUP=0; shift ;;
      --skip-cargo-build) CA_SKIP_CARGO_BUILD=1; shift ;;
      --skip-host-openclaw) CA_SKIP_HOST_OPENCLAW=1; shift ;;
      --rebuild-tools-image) CA_REBUILD_TOOLS_IMAGE=1; shift ;;
      --disable-pep) CA_DISABLE_PEP=1; shift ;;
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
      --bailian-api-key) CA_BAILIAN_API_KEY="${2:?missing value for --bailian-api-key}"; shift 2 ;;
      --bailian-model) CA_BAILIAN_MODEL="${2:?missing value for --bailian-model}"; shift 2 ;;
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
      --build-backend) CA_BUILD_BACKEND="${2:?missing value for --build-backend}"; shift 2 ;;
      --base-image) CA_BASE_IMAGE="${2:?missing value for --base-image}"; shift 2 ;;
      --skip-build) CA_SKIP_BUILD=1; shift ;;
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
      CA_SERVICE_ID="openclaw"
      CA_SERVICE_LABEL="OpenClaw"
      CA_PROJECT_NAME="openclaw"
      CA_SPEC_NAME="openclaw.yaml"
      CA_CONNECT_NAME="connect"
      CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-i}"
      CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.g9i.xlarge}"
      CA_DISK_GB="${CA_DISK_GB:-200}"
      CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-/openclaw}"
      ;;
    openclaw-vllm)
      CA_SERVICE_ID="openclaw-vllm"
      CA_SERVICE_LABEL="OpenClaw vLLM"
      CA_PROJECT_NAME="openclaw-vllm"
      CA_SPEC_NAME="openclaw-vllm.yaml"
      CA_CONNECT_NAME="connect-openclaw-vllm"
      CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-l}"
      CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}"
      CA_DISK_GB="${CA_DISK_GB:-512}"
      CA_CONNECT_READINESS_PATH="${CA_CONNECT_READINESS_PATH:-/openclaw}"
      ;;
    vllm-inference)
      CA_SERVICE_ID="vllm-inference"
      CA_SERVICE_LABEL="Secure LLM Inference"
      CA_PROJECT_NAME="vllm-inference"
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
}

validate_options() {
  case "$CA_REFERENCE_VALUES" in
    sample|rekor) ;;
    *) die "--reference-values must be sample or rekor" ;;
  esac
  case "$CA_BUILD_BACKEND" in
    mkosi) ;;
    base-image)
      [[ -n "$CA_BASE_IMAGE" ]] || die "--base-image is required with --build-backend base-image"
      ;;
    *) die "--build-backend must be mkosi or base-image" ;;
  esac
  [[ "$CA_DISK_GB" =~ ^[0-9]+$ ]] || die "--disk-gb must be an integer"
  [[ -n "$CA_BAILIAN_MODEL" ]] || die "--bailian-model cannot be empty"
  [[ "$CA_OPENCLAW_VLLM_PORT" =~ ^[0-9]+$ ]] || die "--vllm-port must be an integer"
  # Both OpenClaw specs enable A2A, and A2A agent cards require Rekor
  # metadata; fail here instead of after the image build at inject time.
  if [[ "$CA_MODE" == deploy-openclaw* && "$CA_REFERENCE_VALUES" != "rekor" ]]; then
    die "the OpenClaw flows enable A2A, which requires --reference-values rekor"
  fi
  if [[ "$CA_MODE" == "deploy-openclaw-vllm" ]]; then
    validate_openclaw_vllm_build_variants
  fi
  if [[ "$CA_MODE" == "deploy-openclaw-vllm" && "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    die "deploy-openclaw-vllm requires PEP because the TDX attestation skill uses cai-pep attest"
  fi
  if [[ "$CA_MODE" == "deploy-vllm-inference" ]]; then
    [[ -n "$CA_OSS_BUCKET" ]] || die "--oss-bucket is required for vllm-inference"
    [[ -n "$CA_MODEL_KEY_FILE" && -f "$CA_MODEL_KEY_FILE" ]] || die "--model-key-file (existing file) is required for vllm-inference"
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

print_startup_config() {
  log "mode: $CA_MODE"
  log "service: $CA_SERVICE_ID"
  log "source: $ROOT_DIR"
  log "state_dir: $CA_STATE_DIR"
  log "work_dir: $CA_WORK_DIR"
  log "region/zone: $CA_REGION / $CA_ZONE_ID"
  log "instance_type: $CA_INSTANCE_TYPE"
  if [[ "$CA_SERVICE_ID" == "openclaw-vllm" ]]; then
    log "vllm_model: $CA_OPENCLAW_VLLM_MODEL_ID"
    log "vllm_served_model: $CA_OPENCLAW_VLLM_SERVED_MODEL_NAME"
    log "vllm_build_variants: $CA_OPENCLAW_VLLM_BUILD_VARIANTS"
  else
    log "bailian_model: $CA_BAILIAN_MODEL"
  fi
  log "reference_values: $CA_REFERENCE_VALUES"
  log "pep: $([[ "$CA_DISABLE_PEP" == "1" ]] && printf disabled || printf enabled)"
}

main() {
  init_defaults
  parse_args "$@"
  apply_target_defaults
  validate_options
  install -d -m 0700 "$CA_STATE_DIR" "$CA_WORK_DIR"
  print_startup_config

  case "$CA_MODE" in
    deploy-openclaw) run_deploy_openclaw ;;
    deploy-openclaw-vllm) run_deploy_openclaw_vllm ;;
    deploy-vllm-inference) run_deploy_vllm_inference ;;
    install-only) run_install_only ;;
    cleanup) run_cleanup ;;
    cleanup-openclaw-vllm) run_cleanup_openclaw_vllm ;;
    cleanup-vllm-inference) run_cleanup_vllm_inference ;;
    *) die "unsupported mode: $CA_MODE" ;;
  esac
}

main "$@"
