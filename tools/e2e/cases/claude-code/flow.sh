#!/usr/bin/env bash

assert_claude_code_secret_rendering() {
  local dashscope_key="$1"
  local spec="$CLAUDE_CODE_DIR/claude-code.yaml"
  local install_script="$CLAUDE_CODE_DIR/install-claude-code.sh"
  for path in "$spec" "$install_script"; do
    if [[ -n "$dashscope_key" ]] && grep -Fq "$dashscope_key" "$path"; then
      echo "Claude Code rendered file contains secret material: $path" >&2
      return 1
    fi
  done
  grep -Fq "packages: [ca-certificates, curl, git, jq, nodejs, npm, tar, xz]" "$spec"
  grep -Fq "source: $ROOT_DIR/examples/cli-agents/files/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /root/.claude/settings.json" "$spec"
  grep -Fq "target: /root/.claude/skills/tdx-remote-attestation/SKILL.md" "$spec"
  grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
  grep -Fq "scripts: [./install-claude-code.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "source: ./secrets/claude.json" "$spec"
  grep -Fq "target: /root/.claude.json" "$spec"
  grep -Fq "CLAUDE_CODE_VERSION" "$install_script"
  grep -Fq "CLI_AGENT_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh claude-code" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/claude-code/install-claude-code.sh" \
    CLI_AGENT_NODE_VERSION NPM_REGISTRY CLAUDE_CODE_VERSION
  test -s "$CLAUDE_CODE_DIR/secrets/settings.json"
  test -s "$CLAUDE_CODE_DIR/secrets/claude.json"
  record "- Claude Code rendered spec keeps provider/API tokens in remote-attested resources only."
}

claude_code_ssh_info() {
  python3.11 - "$WORK_DIR/status-live.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
item = data[0] if isinstance(data, list) else data
local = item.get("local") or item
cloud = local.get("cloud") or local.get("deploy") or {}
build = local.get("build") or {}
debug = build.get("debug_ssh") or {}
print(cloud.get("public_ip") or cloud.get("private_ip") or "")
print(debug.get("private_key") or "")
PY
}

collect_claude_code_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/claude-code-guest-diagnostics-$label.txt"
  record_cmd "ssh claude-code '<runtime diagnostics>'"
  ssh_guest "$key" "$host" /bin/bash >"$out" 2>&1 <<'REMOTE' || true
set +e
echo "### versions"
node --version
npm --version
claude --version
cai-pep --help | sed -n '1,40p'
echo "### resources"
find /root/.claude /root/.claude.json -maxdepth 3 -printf "%p %m %u:%g %s bytes\n" 2>&1
echo "### claude settings status"
claude --print "/status" --output-format text --max-turns 1 2>&1
REMOTE
  record_file_as_block "Claude Code guest diagnostics ($label):" "$out" text
}

assert_claude_code_guest_runtime() {
  local host="$1"
  local key="$2"
  local out="$WORK_DIR/claude-code-guest-runtime.txt"
  local err="$WORK_DIR/claude-code-guest-runtime.err"
  record_cmd "ssh claude-code '<runtime assertions>'"
  if ssh_guest "$key" "$host" /bin/bash >"$out" 2>"$err" <<'REMOTE'
set -euo pipefail
command -v claude
command -v cai-pep
claude --version
claude --version | grep -Fq "Claude Code"
(cai-pep --help 2>&1 || true) | grep -Fq "cai-pep attest collect-and-verify"
test -s /root/.claude/settings.json
test -s /root/.claude.json
test -s /root/.claude/skills/tdx-remote-attestation/SKILL.md
test "$(stat -c %a /root/.claude/settings.json)" = "600"
grep -Fq "ANTHROPIC_BASE_URL" /root/.claude/settings.json
grep -Fq "ANTHROPIC_AUTH_TOKEN" /root/.claude/settings.json
grep -Fq "ANTHROPIC_MODEL" /root/.claude/settings.json
jq -e '.hasCompletedOnboarding == true' /root/.claude.json >/dev/null
REMOTE
  then
    record_file_as_block "Claude Code guest runtime assertions:" "$out" text
    record_file_as_block "Claude Code guest runtime assertion stderr:" "$err" text
    return 0
  fi
  record_file_as_block "Claude Code guest runtime assertions:" "$out" text
  record_file_as_block "Claude Code guest runtime assertion stderr:" "$err" text
  return 1
}

run_claude_code_prompt() {
  local host="$1"
  local key="$2"
  local label="$3"
  local prompt="$4"
  local expect="$5"
  local max_turns="${6:-4}"
  local out="$WORK_DIR/claude-code-$label.out"
  local err="$WORK_DIR/claude-code-$label.err"
  local remote_prompt
  remote_prompt="$(printf '%q' "$prompt")"
  record_cmd "ssh claude-code 'claude --print <redacted prompt>'"
  if ssh_guest "$key" "$host" "set -euo pipefail
mkdir -p /workspace/claude-code-e2e
cd /workspace/claude-code-e2e
claude --print $remote_prompt --max-turns $max_turns --output-format text --allowedTools Bash" >"$out" 2>"$err"; then
    record_file_as_block "Claude Code $label stdout:" "$out" text
    record_file_as_block "Claude Code $label stderr:" "$err" text
    grep -Fq "$expect" "$out"
    return 0
  fi
  record_file_as_block "Claude Code $label stdout:" "$out" text
  record_file_as_block "Claude Code $label stderr:" "$err" text
  return 1
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_CLAUDE_CODE_DISK_GB:-60}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/claude-code-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  CLAUDE_CODE_DIR="$INIT_OUTPUT_DIR/claude-code"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  DASHSCOPE_ANTHROPIC_BASE_URL="${DASHSCOPE_ANTHROPIC_BASE_URL:-https://dashscope.aliyuncs.com/apps/anthropic}"
  DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-Please respond with CA_E2E_OK and no other text.}"
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
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Claude Code CLI E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local allowed_cidr cosign_key dashscope_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  export COSIGN_KEY="$cosign_key"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB DASHSCOPE_BASE_URL DASHSCOPE_ANTHROPIC_BASE_URL DASHSCOPE_MODEL

  stage_cpu_example claude-code "$CLAUDE_CODE_DIR" "$DISK_GB" "$dashscope_key"
  assert_claude_code_secret_rendering "$dashscope_key"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Claude Code npm version: \`${E2E_CLAUDE_CODE_VERSION:-$(ca_software_version claude-code)}\`."
  record "- CLI agent npm registry: \`${E2E_CLI_AGENT_NPM_REGISTRY:-${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}}\`."

  validate_specs "$STATE_DIR" "$CLAUDE_CODE_DIR/claude-code.yaml"

  register_destroy_target "$STATE_DIR" claude-code

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$CLAUDE_CODE_DIR/claude-code.yaml"
  fi
  record_manifest_variants "$STATE_DIR" claude-code
  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    ca_run "$STATE_DIR" deploy --spec "$CLAUDE_CODE_DIR/claude-code.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" claude-code 1200
  ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  mapfile -t ssh_lines < <(claude_code_ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  assert_claude_code_guest_runtime "$host" "$key"

  if ! run_claude_code_prompt "$host" "$key" chat "$CHAT_MESSAGE" "$CHAT_EXPECT" 4; then
    collect_claude_code_guest_diagnostics "$host" "$key" "chat-failure"
    return 1
  fi

  if [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    local tdx_prompt="/tdx-remote-attestation Run the required remote attestation command and summarize the result. Include CA_TDX_SKILL_OK in the final answer."
    if ! run_claude_code_prompt "$host" "$key" tdx-skill "$tdx_prompt" "$TDX_EXPECT" 8; then
      collect_claude_code_guest_diagnostics "$host" "$key" "tdx-failure"
      return 1
    fi
    grep -Fq "CA_TDX_SKILL_OK" "$WORK_DIR/claude-code-tdx-skill.out"
  fi

  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" claude-code
}
