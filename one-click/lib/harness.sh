#!/usr/bin/env bash

is_supported_harness() {
  case "$1" in
    openclaw|hermes|codex|claude-code|qodercli|opencode|pi|dsh) return 0 ;;
    *) return 1 ;;
  esac
}

resolve_harness_options() {
  if [[ "$CA_ONE_CLICK_TARGET" == openclaw && "$CA_MODE" == deploy-openclaw && "$CA_NON_INTERACTIVE" != 1 ]]; then
    if [[ -z "$CA_HARNESS" ]]; then
      while true; do
        prompt_value CA_HARNESS "Harness (openclaw/hermes/codex/claude-code/qodercli/opencode/pi/dsh)" openclaw
        if is_supported_harness "$CA_HARNESS"; then break; fi
        warn "unsupported harness: $CA_HARNESS"
      done
    fi
    if [[ "$CA_HARNESS" != qodercli ]]; then
      if [[ -z "$CA_MODEL" ]]; then
        prompt_value CA_MODEL "Model" "$(ca_software_version bailian_model)"
      fi
      if [[ -z "$CA_API_BASE_URL" ]]; then
        prompt_value CA_API_BASE_URL "Inference API base URL" "$(default_api_base_url)"
      fi
    fi
  fi
  CA_HARNESS="${CA_HARNESS:-openclaw}"
  CA_MODEL="${CA_MODEL:-$(ca_software_version bailian_model)}"
  CA_API_BASE_URL="${CA_API_BASE_URL:-$(default_api_base_url)}"
}

default_api_base_url() {
  if [[ "${CA_HARNESS:-openclaw}" == claude-code ]]; then
    printf '%s\n' "${DASHSCOPE_ANTHROPIC_BASE_URL:-https://dashscope.aliyuncs.com/apps/anthropic}"
  else
    printf '%s\n' "${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  fi
}

prepare_harness_specs() {
  ensure_gateway_token
  export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN
  local args=()
  if [[ -z "$CA_IMAGE_RELEASE" ]]; then
    local builder_dir="$CA_WORK_DIR/allinone-builder"
    local build_spec="$builder_dir/build.yaml"
    CA_IMAGE_RELEASE="$builder_dir/image-release.json"
    CA_TRUSTED_KEY="$builder_dir/publisher.pem"
    if [[ ! -f "$CA_IMAGE_RELEASE" ]]; then
      ensure_cosign_key
      append_init_common_args args
      (cd "$ROOT_DIR" && ca_cmd init allinone --stage build "${args[@]}")
      if [[ "$CA_SKIP_BUILD" != 1 ]]; then
        ca_cmd build --spec "$build_spec"
      fi
      ca_cmd image publish allinone-builder --spec "$build_spec"
      local signing_key="$CA_WORK_DIR/secrets/publisher.key"
      if [[ ! -f "$signing_key" ]]; then
        (umask 077; openssl genpkey -algorithm ED25519 -out "$signing_key")
      fi
      openssl pkey -in "$signing_key" -pubout -out "$CA_TRUSTED_KEY"
      ca_cmd image export allinone-builder --signing-key "$signing_key" --output "$CA_IMAGE_RELEASE"
    fi
  fi
  args=()
  append_init_common_args args
  args+=(--stage deploy --service-id "$CA_SERVICE_ID" --harness "$CA_HARNESS"
    --harness-mode cli --image-release "$CA_IMAGE_RELEASE" --trusted-key "$CA_TRUSTED_KEY")
  if [[ "$CA_HARNESS" != qodercli ]]; then
    args+=(--model "$CA_MODEL"
      --dashscope-base-url "$CA_API_BASE_URL" --dashscope-anthropic-base-url "$CA_API_BASE_URL")
  fi
  run_confidential_agent_init allinone "${args[@]}"
}

run_deploy_harness() {
  install_os_dependencies
  ensure_aliyun_credentials
  ensure_aliyun_cli_region
  if [[ "$CA_HARNESS" != qodercli ]]; then ensure_bailian_credentials; fi
  resolve_allowed_cidr
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  ensure_native_tools
  prepare_harness_specs
  ensure_operator_peering
  deploy_openclaw_service
  cat <<EOF

Deployment summary
  harness:   $CA_HARNESS (CLI)
  state_dir: $CA_STATE_DIR
  work_dir:  $CA_WORK_DIR
  spec:      $CA_SPEC_PATH
  service:   $CA_SERVICE_ID
  release:   $CA_IMAGE_RELEASE

Cleanup the instance (the shared image is retained):
  one-click/install.sh cleanup --harness "$CA_HARNESS" --state-dir "$CA_STATE_DIR" --work-dir "$CA_WORK_DIR"
EOF
  if [[ "$CA_HARNESS" == dsh ]]; then
    cat <<EOF

Run a DSH task:
  confidential-agent --tools-dir "$CA_TOOLS_DIR" --state-dir "$CA_STATE_DIR" ssh "$CA_SERVICE_ID" -- cai-agent headless 'Your task'
EOF
  else
    cat <<EOF

Start an interactive session:
  confidential-agent --tools-dir "$CA_TOOLS_DIR" --state-dir "$CA_STATE_DIR" ssh "$CA_SERVICE_ID" --tty -- cai-agent
EOF
  fi
  if [[ "$CA_HARNESS" == qodercli ]]; then
    log "Sign in to your Qoder account in the interactive session."
  fi
}
