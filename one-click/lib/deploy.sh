#!/usr/bin/env bash

ensure_shelter() {
  case "${CA_MODE:-}" in
    cleanup|cleanup-openclaw-vllm|cleanup-vllm-inference)
      ensure_shelter_cleanup
      return
      ;;
  esac
  if [[ "${CA_PACKAGED_INSTALL:-0}" == 1 && -z "${CA_SHELTER_RPM:-}" ]]; then
    export CA_SHELTER_BIN="${CA_SHELTER_BIN:-/usr/bin/shelter}"
    return
  fi
  local shelter_rpm=""
  if [[ -n "${CA_SHELTER_BIN:-}" ]]; then
    [[ -x "$CA_SHELTER_BIN" ]] || die "CA_SHELTER_BIN is not executable: $CA_SHELTER_BIN"
    export CA_SHELTER_BIN
  else
    shelter_rpm="$(resolve_shelter_rpm)"
    CA_SHELTER_BIN="$(command -v shelter || true)"
    if [[ -z "$CA_SHELTER_BIN" ]] ||
       ! "$ROOT_DIR/hack/check-shelter.sh" "$CA_SHELTER_BIN" >/dev/null 2>&1 ||
       ! "$ROOT_DIR/hack/check-shelter-rpm.sh" "$CA_SHELTER_BIN" "$shelter_rpm" >/dev/null 2>&1; then
      install_shelter_rpm "$shelter_rpm"
      CA_SHELTER_BIN=/usr/bin/shelter
    fi
    export CA_SHELTER_BIN
  fi

  if [[ -n "$shelter_rpm" ]]; then
    "$ROOT_DIR/hack/check-shelter-rpm.sh" "$CA_SHELTER_BIN" "$shelter_rpm" || die "Shelter RPM does not satisfy the bundled package version"
  fi
  "$ROOT_DIR/hack/check-shelter.sh" "$CA_SHELTER_BIN" || die "Shelter is incompatible; install the bundled RPM or set CA_SHELTER_BIN to a compatible binary"
  "$CA_SHELTER_BIN" --version >/dev/null
  log "using Shelter: $CA_SHELTER_BIN"
  if [[ "$CA_REFERENCE_VALUES" == rekor && ! -x "$CA_SLSA_GENERATOR" ]] && needs_local_image_build; then
    die "SLSA generator is required for rekor mode: $CA_SLSA_GENERATOR"
  fi
}

ensure_shelter_cleanup() {
  if [[ "${CA_PACKAGED_INSTALL:-0}" == 1 ]]; then CA_SHELTER_BIN="${CA_SHELTER_BIN:-/usr/bin/shelter}"; fi
  CA_SHELTER_BIN="${CA_SHELTER_BIN:-$(command -v shelter || true)}"
  [[ -n "$CA_SHELTER_BIN" && -x "$CA_SHELTER_BIN" ]] ||
    die "Shelter is required for cleanup; install a compatible CLI or set CA_SHELTER_BIN"
  export CA_SHELTER_BIN
  "$ROOT_DIR/hack/check-shelter.sh" "$CA_SHELTER_BIN" --cleanup ||
    die "Shelter lacks the managed removal/network commands required for cleanup"
  "$CA_SHELTER_BIN" --version >/dev/null
  log "using Shelter for cleanup: $CA_SHELTER_BIN"
}

resolve_shelter_rpm() {
  if [[ -n "${CA_SHELTER_RPM:-}" ]]; then
    [[ -f "$CA_SHELTER_RPM" ]] || die "Shelter RPM does not exist: $CA_SHELTER_RPM"
    printf '%s\n' "$CA_SHELTER_RPM"
    return
  fi

  "$ROOT_DIR/hack/resolve-shelter-rpm.sh"
}

assert_shelter_upgrade_idle() {
  local executable process pid
  for executable in "$@"; do
    [[ -f "$executable" ]] || continue
    for process in /proc/[0-9]*/exe; do
      if [[ "$process" -ef "$executable" ]]; then
        pid="${process#/proc/}"
        die "Shelter upgrade blocked by running process ${pid%/exe} using $executable; finish or stop the active Shelter operation before installing the RPM"
        return 1
      fi
    done
  done
}

install_shelter_rpm() {
  local rpm="$1"
  local pm
  is_root || die "Shelter is missing and installing the Shelter RPM requires root. Re-run as root, install Shelter first, or set CA_SHELTER_BIN."
  pm="$(package_manager)" || die "yum or dnf is required to install the Shelter RPM"
  assert_shelter_upgrade_idle /usr/bin/shelter /usr/bin/shelter-slim
  log "installing Shelter RPM: $rpm"
  local required installed operation=install
  required="$(rpm -qp --qf '%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}' "$rpm")"
  installed="$(rpm -q --qf '%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}' shelter 2>/dev/null || true)"
  [[ "$required" != "$installed" ]] || operation=reinstall
  "$pm" "$operation" -y "$rpm"
  if [[ "${CA_MODE:-}" != deploy-openclaw || "${CA_IMAGE_SOURCE:-prebuilt}" == build ]]; then
    "$pm" install -y libguestfs-tools-c
  fi
}

needs_pep_binary() {
  [[ "${CA_DISABLE_PEP:-0}" != 1 ]] ||
    [[ "${CA_MODE:-}" == deploy-openclaw && "${CA_IMAGE_SOURCE:-}" == build ]]
}

build_confidential_agent() {
  if [[ "${CA_PACKAGED_INSTALL:-0}" == 1 ]]; then return; fi
  if [[ "${CA_SKIP_CARGO_BUILD:-0}" == 1 ]]; then
    [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
    [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
    [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary is missing: $CA_GATEWAY_BIN"
    if needs_pep_binary; then
      [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary is missing: $CA_PEP_BIN"
    fi
    return
  fi
  ensure_rust_toolchain
  if needs_pep_binary; then
    log "building Confidential Agent host CLI, guest daemon, gateway and PEP"
    (cd "$ROOT_DIR" && cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep)
  else
    log "building Confidential Agent host CLI, guest daemon and gateway"
    (cd "$ROOT_DIR" && cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway)
  fi
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary was not built: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary was not built: $CA_AGENTD_BIN"
  [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary was not built: $CA_GATEWAY_BIN"
  if needs_pep_binary; then
    [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary was not built: $CA_PEP_BIN"
  fi
}

install_confidential_agent_cli() {
  if [[ "${CA_INSTALL_CLI:-1}" != "1" ]]; then
    return 0
  fi
  if [[ "${CA_PACKAGED_INSTALL:-0}" == "1" ]]; then
    export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN
    log "using packaged Confidential Agent binaries: $CA_BIN"
    return 0
  fi
  is_root || die "installing confidential-agent to /usr/local/bin requires root"
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
  [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary is missing: $CA_GATEWAY_BIN"
  local cli_dest="/usr/local/bin/confidential-agent"
  local agentd_dest="/usr/local/bin/confidential-agentd"
  local gateway_dest="/usr/local/bin/cai-gateway"
  # Inspect using the candidate CLI before replacing the working host binaries.
  "$CA_BIN" --state-dir "$CA_STATE_DIR" upgrade check || die "host upgrade blocked by legacy deployment inputs; retain the installed CLI and migrate the affected state first"
  install -D -m 0755 "$CA_BIN" "$cli_dest"
  install -D -m 0755 "$CA_AGENTD_BIN" "$agentd_dest"
  install -D -m 0755 "$CA_GATEWAY_BIN" "$gateway_dest"
  CA_BIN="$cli_dest"
  CA_AGENTD_BIN="$agentd_dest"
  CA_GATEWAY_BIN="$gateway_dest"
  if needs_pep_binary; then
    [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary is missing: $CA_PEP_BIN"
    local pep_dest="/usr/local/bin/cai-pep"
    install -D -m 0755 "$CA_PEP_BIN" "$pep_dest"
    CA_PEP_BIN="$pep_dest"
    export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN
    log "installed Confidential Agent binaries: $CA_BIN, $CA_AGENTD_BIN, $CA_GATEWAY_BIN, $CA_PEP_BIN"
  else
    export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN
    log "installed Confidential Agent binaries: $CA_BIN, $CA_AGENTD_BIN, $CA_GATEWAY_BIN"
  fi
}

ensure_native_tools() {
  if [[ "${CA_PACKAGED_INSTALL:-0}" == 1 ]]; then return; fi
  log "installing native host tools"
  CA_TOOLS_DIR="$CA_TOOLS_DIR" "$ROOT_DIR/packaging/install-native-tools.sh"
}

ca_cmd() {
  "$CA_BIN" --tools-dir "$CA_TOOLS_DIR" --state-dir "$CA_STATE_DIR" "$@"
}

confirm_operator_peering_entry() {
  local label="$1" cidr="$2" existing
  existing="$(ca_cmd peering show "$label" 2>/dev/null)" || return 0
  if grep -Fxq "cidr: $cidr" <<<"$existing"; then return; fi
  if [[ "$CA_NON_INTERACTIVE" == 1 ]]; then
    die "peering '$label' already exists with a different CIDR. Re-run with --yes to replace it."
  fi
  confirm "Replace operator peering '$label' with $cidr?" "y" ||
    die "operator peering replacement was declined"
  CA_APPROVED_PEERING_REPLACEMENTS+=" $label"
}

ensure_operator_peering_entry() {
  local label="$1"
  local cidr="$2"
  local note="$3"
  local out="$CA_WORK_DIR/peering-$label.txt"
  if ca_cmd peering show "$label" >"$out" 2>/dev/null; then
    if grep -Fxq "cidr: $cidr" "$out"; then
      log "operator peering '$label' already allows $cidr"
      return
    fi
    if [[ "$CA_ASSUME_YES" != 1 && " ${CA_APPROVED_PEERING_REPLACEMENTS:-} " != *" $label "* ]]; then
      die "peering '$label' already exists with a different CIDR. Approve replacement before deployment."
    fi
    ca_cmd peering remove "$label"
  fi
  log "adding operator peering '$label' for $cidr"
  ca_cmd peering add --role operator --cidr "$cidr" --label "$label" --note "$note"
}

operator_peering_entries() {
  local operation="$1"
  local allowed_cidrs="${CA_ALLOWED_CIDRS:-${CA_ALLOWED_CIDR:-}}"
  local cidr label idx=0
  [[ -n "$allowed_cidrs" ]] || die "operator CIDR list is empty"
  for cidr in $allowed_cidrs; do
    idx=$((idx + 1))
    if [[ "$idx" == "1" ]]; then
      label="ops"
    else
      label="ops-$idx"
    fi
    "$operation" "$label" "$cidr" "operator access CIDR selected by one-click installer"
  done
  if [[ -n "${CA_DEPLOYER_CIDR:-}" ]] \
    && ! cidr_list_contains "$allowed_cidrs" "$CA_DEPLOYER_CIDR" \
    && ! cidr_list_contains "$allowed_cidrs" "0.0.0.0/0"; then
    "$operation" deployer "$CA_DEPLOYER_CIDR" "deployment host egress CIDR detected by one-click installer"
  fi
}

prepare_operator_peering() {
  CA_APPROVED_PEERING_REPLACEMENTS=""
  if [[ "$CA_ASSUME_YES" != 1 && -f "$CA_STATE_DIR/peerings.yaml" ]]; then
    local inspect_bin="$CA_BIN"
    if [[ ! -x "$inspect_bin" ]]; then
      inspect_bin="$(command -v confidential-agent)" ||
        die "reviewing existing operator peerings requires an installed confidential-agent CLI; use --yes to approve replacement"
    fi
    CA_BIN="$inspect_bin" operator_peering_entries confirm_operator_peering_entry
  fi
}

ensure_operator_peering() {
  install -d -m 0700 "$CA_WORK_DIR"
  operator_peering_entries ensure_operator_peering_entry
}

build_openclaw_image() {
  if [[ "$CA_SKIP_BUILD" == "1" ]]; then
    log "skipping $CA_SERVICE_LABEL image build"
    return
  fi
  local phase
  phase="$(service_status | jq -er '.phase' || true)"
  if [[ "$phase" == "deployed" || "$phase" == "active" ]]; then
    log "$CA_SERVICE_LABEL is $phase; skipping image build (existing deployment detected)"
    return
  fi
  log "building $CA_SERVICE_LABEL confidential image"
  (cd "$CA_PROJECT_DIR" && ca_cmd build --spec "$CA_SPEC_PATH")
}

service_status() {
  ca_cmd status --service "$CA_SERVICE_ID" --json 2>/dev/null | jq -er '.[0]'
}

deploy_openclaw_service() {
  if [[ "$CA_SKIP_DEPLOY" == "1" ]]; then
    log "skipping $CA_SERVICE_LABEL deploy"
    return
  fi
  local status
  status="$(service_status || true)"
  if jq -e '.phase == "active" and .cloud.present' <<<"${status:-null}" >/dev/null; then
    log "$CA_SERVICE_LABEL is already active; skipping cloud deploy"
    CA_DEPLOY_SKIPPED_ACTIVE=1
    return
  fi
  if [[ "$(jq -r '.phase' <<<"$status")" == deployed ]]; then
    local public_ip
    public_ip="$(jq -er '.cloud.public_ip' <<<"$status")" || die "$CA_SERVICE_LABEL is deployed but has no recorded public IP; run 'confidential-agent destroy $CA_SERVICE_ID' and redeploy"
    log "$CA_SERVICE_LABEL is deployed but not active; resuming from resource injection to $public_ip"
    (cd "$CA_PROJECT_DIR" && ca_cmd inject --spec "$CA_SPEC_PATH" --target-ip "$public_ip")
    return
  fi
  log "deploying $CA_SERVICE_LABEL to Aliyun"
  (cd "$CA_PROJECT_DIR" && ca_cmd deploy --spec "$CA_SPEC_PATH")
}

sync_active_openclaw_resources() {
  if [[ "${CA_DEPLOY_SKIPPED_ACTIVE:-0}" != "1" || "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  local public_ip
  public_ip="$(service_status | jq -er '.cloud.public_ip')" || {
    warn "could not determine active $CA_SERVICE_LABEL public IP; skipping resource sync"
    return
  }
  log "syncing resources to active $CA_SERVICE_LABEL at $public_ip"
  (cd "$CA_PROJECT_DIR" && ca_cmd inject --spec "$CA_SPEC_PATH" --target-ip "$public_ip")
  CA_ACTIVE_RESOURCES_SYNCED=1
}

restart_active_openclaw_app() {
  if [[ "${CA_ACTIVE_RESOURCES_SYNCED:-0}" != "1" || "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  if ! command -v ssh >/dev/null 2>&1; then
    warn "ssh is not installed; skipping active OpenClaw gateway restart"
    return
  fi
  local public_ip key known_hosts
  public_ip="$(service_status | jq -er '.cloud.public_ip')" || {
    warn "could not determine active OpenClaw public IP; skipping gateway restart"
    return
  }
  key="$(service_status | jq -er '.build.debug_ssh.private_key')" || {
    warn "could not determine debug SSH key; skipping gateway restart"
    return
  }
  [[ -f "$key" ]] || {
    warn "debug SSH key is missing: $key"
    return
  }
  known_hosts="$CA_WORK_DIR/known_hosts"
  log "restarting active OpenClaw gateway service"
  if ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" -i "$key" "root@$public_ip" "systemctl restart cai-openclaw-gateway.service"; then
    sleep 5
  else
    warn "failed to restart cai-openclaw-gateway.service over debug SSH"
  fi
}

wait_for_live_status() {
  if [[ "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  local deadline=$((SECONDS + CA_STATUS_TIMEOUT_SEC))
  local status_json="$CA_WORK_DIR/status-live.json"
  log "waiting for live guest status"
  while ((SECONDS < deadline)); do
    if ca_cmd status --service "$CA_SERVICE_ID" --live --json >"$status_json" 2>"$CA_WORK_DIR/status-live.err"; then
      if jq -e '.[0].daemon.app_ready' "$status_json" >/dev/null; then
        log "live status is available"
        return
      fi
    fi
    sleep 10
  done
  if [[ "$CA_MODE" == deploy-openclaw ]]; then
    die "guest did not become ready within ${CA_STATUS_TIMEOUT_SEC}s; see $status_json and $CA_WORK_DIR/status-live.err"
  fi
  warn "live status did not become ready within ${CA_STATUS_TIMEOUT_SEC}s; see $CA_WORK_DIR/status-live.err"
}

expected_connect_guest_port() {
  case "${CA_ONE_CLICK_TARGET:-openclaw}" in
    vllm-inference) printf '%s\n' "$CA_VLLM_INFERENCE_PORT" ;;
    openclaw-vllm) printf '18789\n' ;;
    openclaw) harness_guest_port ;;
  esac
}

probe_codex_websocket() {
  python3.11 - "$CA_CONNECT_HOST" "$CA_CONNECT_PORT" "$CA_PROJECT_DIR/secrets/app-server-token" <<'PROBE'
import base64, http.client, os, sys
from pathlib import Path

client = http.client.HTTPConnection(sys.argv[1], int(sys.argv[2]), timeout=5)
client.request("GET", "/", headers={
    "Connection": "Upgrade", "Upgrade": "websocket", "Sec-WebSocket-Version": "13",
    "Sec-WebSocket-Key": base64.b64encode(os.urandom(16)).decode(),
    "Authorization": "Bearer " + Path(sys.argv[3]).read_text().strip(),
})
response = client.getresponse()
client.close()
sys.exit(0 if response.status == 101 else 1)
PROBE
}

probe_connect_endpoint() {
  local url="http://$CA_CONNECT_HOST:$CA_CONNECT_PORT" path="${CA_CONNECT_READINESS_PATH:-}"
  local -a options=(-fsSL --max-time 5)
  case "${CA_ONE_CLICK_TARGET:-openclaw}:${CA_HARNESS:-openclaw}" in
    openclaw:codex) probe_codex_websocket; return ;;
    openclaw:dsh)
      CA_DSH_WEB_URL="$(ca_cmd ssh "$CA_SERVICE_ID" -- cai-agent --web-url --port "$CA_CONNECT_PORT")" || return
      curl "${options[@]}" -b '' "$CA_DSH_WEB_URL" >/dev/null
      return ;;
    openclaw:hermes)
      options+=(-H "Authorization: Bearer $(jq -er .environment.API_SERVER_KEY "$CA_PROJECT_DIR/secrets/runtime.json")")
      path="${path:-/v1/models}" ;;
    openclaw:opencode)
      options+=(-u "opencode:$(jq -er .environment.OPENCODE_SERVER_PASSWORD "$CA_PROJECT_DIR/secrets/runtime.json")")
      path="${path:-/global/health}" ;;
    openclaw:openclaw|openclaw-vllm:*) options+=(-H "Authorization: Bearer $CA_GATEWAY_TOKEN") ;;
  esac
  curl "${options[@]}" "$url$path" >/dev/null
}

start_connect() {
  if [[ "$CA_SKIP_DEPLOY" == 1 || "$CA_START_CONNECT" != 1 ]]; then return; fi
  local ready="$CA_READY_JSON" log_path="$CA_WORK_DIR/$CA_CONNECT_NAME.log" endpoint
  if [[ -f "$ready" ]]; then ca_cmd connect stop --ready-json "$ready"; fi
  ca_cmd connect start --service "$CA_SERVICE_ID" --ready-json "$ready" --log-file "$log_path" --wait-ready "$CA_CONNECT_TIMEOUT_SEC" || return
  endpoint="$(jq -er --argjson port "$(expected_connect_guest_port)" '
    .process.pid as $pid | .client_endpoints[] | select(.guest_port == $port) |
    [.local_host,.local_port,$pid] | @tsv' "$ready")" || return
  IFS=$'\t' read -r CA_CONNECT_HOST CA_CONNECT_PORT CA_CONNECT_PID <<<"$endpoint"
  local deadline=$((SECONDS + CA_CONNECT_TIMEOUT_SEC))
  until probe_connect_endpoint; do
    if ((SECONDS >= deadline)); then
      ca_cmd connect stop --ready-json "$ready"
      die "service endpoint did not become ready at $CA_CONNECT_HOST:$CA_CONNECT_PORT; see $log_path"
    fi
    sleep 2
  done
  log "$CA_SERVICE_ID is reachable through TNG at $CA_CONNECT_HOST:$CA_CONNECT_PORT"
}

run_web_smoke() {
  if [[ -z "${CA_CONNECT_PORT:-}" ]]; then
    return
  fi
  local host="${CA_CONNECT_HOST:-127.0.0.1}"
  local body="$CA_WORK_DIR/web-smoke.html"
  log "verifying OpenClaw control UI through connect"
  if ! curl -fsSL --max-time 15 "http://$host:$CA_CONNECT_PORT/openclaw" -o "$body"; then
    warn "control UI fetch failed; see $body"
    return
  fi
  if ! grep -qiE 'openclaw|control[ -]?ui' "$body"; then
    warn "control UI body did not contain OpenClaw branding; see $body"
    return
  fi
  log "control UI content check passed"
}

stop_connect() {
  if [[ -f "$CA_READY_JSON" ]]; then
    ca_cmd connect stop --ready-json "$CA_READY_JSON"
  fi
}

print_summary() {
  local host="${CA_CONNECT_HOST:-127.0.0.1}"
  local connect_pid="${CA_CONNECT_PID:-}"
  local connect_name="${CA_CONNECT_NAME:-connect}"
  local ready_json="${CA_READY_JSON:-$CA_WORK_DIR/$connect_name.json}"
  local log_path="$CA_WORK_DIR/$connect_name.log"

  cat <<EOF

Deployment summary
  state_dir: $CA_STATE_DIR
  work_dir:  $CA_WORK_DIR
  spec:      $CA_SPEC_PATH
  service:   $CA_SERVICE_ID
  region:    $CA_REGION
  zone_id:   $CA_ZONE_ID
  instance:  $CA_INSTANCE_TYPE
  cidrs:     ${CA_ALLOWED_CIDRS:-${CA_ALLOWED_CIDR:-not set}}
  deployer:  ${CA_DEPLOYER_CIDR:-not detected}
  token:     $CA_GATEWAY_TOKEN
EOF
  if [[ "$CA_SERVICE_ID" != "vllm-inference" ]]; then
    cat <<EOF
  dingtalk:  $CA_ENABLE_DINGTALK
  pep:       $([[ "${CA_DISABLE_PEP:-0}" == "1" ]] && printf disabled || printf enabled)
EOF
  fi
  if [[ -n "${CA_CONNECT_PORT:-}" ]]; then
    if [[ "$CA_SERVICE_ID" == "vllm-inference" ]]; then
      cat <<EOF
  vllm:      curl http://$host:$CA_CONNECT_PORT/v1/models
  chat:      curl http://$host:$CA_CONNECT_PORT/v1/chat/completions \\
                 -H 'Content-Type: application/json' \\
                 -d '{"model":"$CA_VLLM_INFERENCE_SERVED_MODEL_NAME","messages":[{"role":"user","content":"hi"}]}'
  ready:     $ready_json
  connect:   running (pid ${connect_pid:-unknown}, log $log_path)
EOF
    else
      cat <<EOF
  web:       http://$host:$CA_CONNECT_PORT/openclaw
  ws/api:    ws://$host:$CA_CONNECT_PORT
  tui:       openclaw tui --url ws://$host:$CA_CONNECT_PORT --token "$CA_GATEWAY_TOKEN"
  ready:     $ready_json
  connect:   running (pid ${connect_pid:-unknown}, log $log_path)
EOF
    fi
  else
    cat <<EOF
  connect:   run this when the service is active:
             confidential-agent connect start --service $CA_SERVICE_ID --ready-json $ready_json
EOF
  fi
  if [[ -n "${CA_CONNECT_PORT:-}" ]]; then
    cat <<EOF

Cleanup:
  confidential-agent destroy $CA_SERVICE_ID
  confidential-agent connect stop --ready-json $ready_json  # stop local tunnel only

EOF
  else
    cat <<EOF

Cleanup:
  confidential-agent destroy $CA_SERVICE_ID

EOF
  fi
}

run_install_only() {
  install_os_dependencies
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  ensure_native_tools
  log "install-only completed"
}

run_deploy_openclaw_vllm() {
  install_os_dependencies
  ensure_host_openclaw_runtime
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  ensure_native_tools
  configure_aliyun_credentials
  prepare_openclaw_vllm_specs
  build_openclaw_image
  ensure_operator_peering
  deploy_openclaw_service
  sync_active_openclaw_resources
  restart_active_openclaw_app
  wait_for_live_status
  start_connect
  run_web_smoke
  print_summary
}

run_deploy_vllm_inference() {
  install_os_dependencies
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  ensure_native_tools
  configure_aliyun_credentials
  prepare_vllm_inference_specs
  build_openclaw_image
  ensure_operator_peering
  deploy_openclaw_service
  sync_active_openclaw_resources
  wait_for_live_status
  start_connect
  print_summary
}

run_cleanup_service() {
  ensure_aliyun_cli_region
  configure_aliyun_credentials
  ensure_shelter
  if [[ -x "$CA_BIN" ]]; then
    log "using Confidential Agent CLI for cleanup: $CA_BIN"
  else
    build_confidential_agent
  fi
  if [[ ! -d "$CA_STATE_DIR" ]]; then
    die "state dir does not exist: $CA_STATE_DIR"
  fi
  stop_connect
  ca_cmd destroy "$CA_SERVICE_ID"
  log "cloud resources for $CA_SERVICE_ID destroy requested"
  log "local state remains at $CA_STATE_DIR; remove it manually only after confirming no resources are needed"
}

run_cleanup() {
  run_cleanup_service
}

run_cleanup_openclaw_vllm() {
  run_cleanup_service
}

run_cleanup_vllm_inference() {
  run_cleanup_service
}
