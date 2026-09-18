#!/usr/bin/env bash

ensure_gateway_token() {
  install -d -m 0700 "$CA_WORK_DIR/secrets"
  local token_file="$CA_WORK_DIR/secrets/gateway.token"
  if [[ -n "${CA_GATEWAY_TOKEN:-}" ]]; then
    if ((${#CA_GATEWAY_TOKEN} < 32)); then
      die "--gateway-token must be at least 32 characters"
    fi
    printf '%s\n' "$CA_GATEWAY_TOKEN" >"$token_file"
    chmod 0600 "$token_file"
    return
  fi
  if [[ -f "$token_file" ]]; then
    CA_GATEWAY_TOKEN="$(tr -d '[:space:]' <"$token_file")"
    if ((${#CA_GATEWAY_TOKEN} < 32)); then
      die "stored gateway token is too short: $token_file"
    fi
    return
  fi
  require_cmd openssl
  CA_GATEWAY_TOKEN="$(openssl rand -hex 20)"
  printf '%s\n' "$CA_GATEWAY_TOKEN" >"$token_file"
  chmod 0600 "$token_file"
}

ensure_cosign_key() {
  if [[ "${CA_REFERENCE_VALUES:-rekor}" != "rekor" ]]; then
    return
  fi
  install -d -m 0700 "$CA_WORK_DIR/secrets"
  if [[ -n "${CA_COSIGN_KEY:-}" ]]; then
    [[ -f "$CA_COSIGN_KEY" ]] || die "cosign key does not exist: $CA_COSIGN_KEY"
    return
  fi
  local prefix="$CA_WORK_DIR/secrets/cosign"
  if [[ ! -f "$prefix.key" ]]; then
    log "generating local cosign key pair with confidential-agent tools image"
    ca_cmd key generate-cosign --output-key-prefix "$prefix" >/dev/null
    chmod 0600 "$prefix.key" "$prefix.pub" 2>/dev/null || true
  fi
  CA_COSIGN_KEY="$prefix.key"
}

append_init_common_args() {
  local -n args_ref="$1"
  args_ref+=(
    --non-interactive
    --force
    --output-dir "$CA_WORK_DIR"
    --region "$CA_REGION"
    --zone-id "$CA_ZONE_ID"
    --instance-type "$CA_INSTANCE_TYPE"
    --disk-gb "$CA_DISK_GB"
    --reference-values "$CA_REFERENCE_VALUES"
    --gateway-token "$CA_GATEWAY_TOKEN"
    --openclaw-version "$CA_OPENCLAW_VERSION"
    --node-version "$CA_NODE_VERSION"
    --npm-registry "$CA_NPM_REGISTRY"
    --slsa-generator "$CA_SLSA_GENERATOR"
    --build-backend "$CA_BUILD_BACKEND"
  )
  if [[ "$CA_BUILD_BACKEND" == "base-image" ]]; then
    args_ref+=(--base-image "$CA_BASE_IMAGE")
  fi
  if [[ "$CA_REFERENCE_VALUES" == "rekor" ]]; then
    args_ref+=(--cosign-key "$CA_COSIGN_KEY")
  fi
  if [[ "$CA_ENABLE_DINGTALK" == "1" ]]; then
    args_ref+=(
      --enable-dingtalk
      --dingtalk-client-id "$DINGTALK_BOT_CLIENT_ID"
      --dingtalk-client-secret "$DINGTALK_BOT_CLIENT_SECRET"
    )
  fi
}

append_openclaw_bailian_args() {
  local -n args_ref="$1"
  args_ref+=(
    --dashscope-api-key "$DASHSCOPE_API_KEY"
    --dashscope-base-url "${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
    --model "$CA_BAILIAN_MODEL"
  )
  if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    args_ref+=(--disable-pep)
  fi
}

append_openclaw_vllm_args() {
  local -n args_ref="$1"
  args_ref+=(
    --vllm-model-id "$CA_OPENCLAW_VLLM_MODEL_ID"
    --vllm-model-dir "$CA_OPENCLAW_VLLM_MODEL_DIR"
    --vllm-served-model-name "$CA_OPENCLAW_VLLM_SERVED_MODEL_NAME"
    --vllm-port "$CA_OPENCLAW_VLLM_PORT"
    --vllm-version "$CA_OPENCLAW_VLLM_VERSION"
    --vllm-build-variants "$CA_OPENCLAW_VLLM_BUILD_VARIANTS"
  )
}

run_confidential_agent_init() {
  local target="$1"
  shift
  export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN
  install -d -m 0700 "$CA_WORK_DIR"
  log "generating $CA_SERVICE_LABEL AppSpec with confidential-agent init $target"
  (cd "$ROOT_DIR" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" init "$target" "$@")
  [[ -f "$CA_SPEC_PATH" ]] || die "init did not create expected AppSpec: $CA_SPEC_PATH"
}

prepare_openclaw_specs() {
  ensure_gateway_token
  ensure_cosign_key
  local args=()
  append_init_common_args args
  append_openclaw_bailian_args args
  run_confidential_agent_init openclaw "${args[@]}"
  log "generated OpenClaw spec at $CA_SPEC_PATH"
}

prepare_openclaw_vllm_specs() {
  ensure_gateway_token
  ensure_cosign_key
  local args=()
  append_init_common_args args
  append_openclaw_vllm_args args
  run_confidential_agent_init openclaw-vllm "${args[@]}"
  log "generated OpenClaw vLLM spec at $CA_SPEC_PATH"
}

append_vllm_inference_args() {
  local -n args_ref="$1"
  args_ref+=(
    --vllm-port "$CA_VLLM_INFERENCE_PORT"
    --vllm-version "$CA_VLLM_INFERENCE_VERSION"
    --vllm-served-model-name "$CA_VLLM_INFERENCE_SERVED_MODEL_NAME"
    --oss-bucket "$CA_OSS_BUCKET"
    --oss-endpoint "$CA_OSS_ENDPOINT"
    --model-key-file "$CA_MODEL_KEY_FILE"
  )
}

prepare_vllm_inference_specs() {
  ensure_gateway_token
  ensure_cosign_key
  local args=()
  append_init_common_args args
  append_vllm_inference_args args
  run_confidential_agent_init vllm-inference "${args[@]}"
  log "generated vllm-inference spec at $CA_SPEC_PATH"
}
