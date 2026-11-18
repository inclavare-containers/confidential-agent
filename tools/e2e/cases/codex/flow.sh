#!/usr/bin/env bash

# shellcheck source=tools/e2e/lib/codex.sh
source "$ROOT_DIR/tools/e2e/lib/codex.sh"

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_CODEX_DISK_GB:-60}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/codex-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  CODEX_DIR="$INIT_OUTPUT_DIR/codex"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
  CODEX_REMOTE_PORT="${E2E_CODEX_REMOTE_PORT:-4500}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-Reply with CA_E2E_OK and no other text.}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"
  TDX_EXPECT="${E2E_TDX_EXPECT:-TDX}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  require_cmd ssh
  require_cmd timeout
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Codex CLI E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local allowed_cidr cosign_key dashscope_key remote_token
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" && -s "$CODEX_DIR/secrets/app-server-token" ]]; then
    remote_token="$(<"$CODEX_DIR/secrets/app-server-token")"
  else
    remote_token="$(openssl rand -base64 32)"
  fi
  export COSIGN_KEY="$cosign_key"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB DASHSCOPE_BASE_URL DASHSCOPE_MODEL CODEX_REMOTE_PORT

  stage_cpu_example codex "$CODEX_DIR" "$DISK_GB" "$dashscope_key" "$remote_token"
  assert_codex_secret_rendering "$dashscope_key" "$remote_token"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Codex npm version: \`${E2E_CODEX_VERSION:-$(ca_software_version codex)}\`."
  record "- CLI agent npm registry: \`${E2E_CLI_AGENT_NPM_REGISTRY:-${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}}\`."
  record "- Codex app-server token generated but not printed."

  validate_specs "$STATE_DIR" "$CODEX_DIR/codex.yaml"

  register_destroy_target "$STATE_DIR" codex

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$CODEX_DIR/codex.yaml"
  fi
  record_manifest_variants "$STATE_DIR" codex
  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    ca_run "$STATE_DIR" deploy --spec "$CODEX_DIR/codex.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" codex 1200
  ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  mapfile -t ssh_lines < <(codex_ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  assert_codex_guest_runtime "$host" "$key"

  if ! run_codex_exec_prompt "$host" "$key" chat "$CHAT_MESSAGE" "$CHAT_EXPECT"; then
    collect_codex_guest_diagnostics "$host" "$key" "chat-failure"
    return 1
  fi

  if [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    local tdx_prompt='Use $tdx-remote-attestation to run the required remote attestation command and summarize the result. Include CA_TDX_SKILL_OK in the final answer.'
    if ! run_codex_exec_prompt "$host" "$key" tdx-skill "$tdx_prompt" "$TDX_EXPECT"; then
      collect_codex_guest_diagnostics "$host" "$key" "tdx-failure"
      return 1
    fi
    grep -Fq "CA_TDX_SKILL_OK" "$WORK_DIR/codex-tdx-skill.out"
    grep -Eq "cai-pep|collect-and-verify|attest" "$WORK_DIR/codex-tdx-skill.out"
  fi

  record "- Guest self-attestation vector matches host CLI verification (all-pass)."
  record_cmd "ssh codex 'cai-pep attest collect-and-verify --claims'"
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "root@$host" \
    "cai-pep attest collect-and-verify --aa-url http://localhost:8006 --tee tdx --policy default --claims" \
    >"$WORK_DIR/guest-self-attest.out" 2>&1
  grep -Eq '"hardware":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"executables":[[:space:]]*3' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"configuration":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"file-system":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"

  local connect_port
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" codex /readyz 4 180 --service codex)"
  record "Connect mapped Codex app-server to \`127.0.0.1:$connect_port\`."
  run_codex_remote_probe "$connect_port" "$remote_token"
  wait_for_status_service_ready "$STATE_DIR" codex 300

  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" codex
}
