#!/usr/bin/env bash

ensure_shelter() {
  if [[ -n "${CA_SHELTER_BIN:-}" ]]; then
    [[ -x "$CA_SHELTER_BIN" ]] || die "CA_SHELTER_BIN is not executable: $CA_SHELTER_BIN"
    export CA_SHELTER_BIN
  elif command -v shelter >/dev/null 2>&1; then
    CA_SHELTER_BIN="$(command -v shelter)"
    export CA_SHELTER_BIN
  else
    local shelter_rpm
    shelter_rpm="$(resolve_shelter_rpm)"
    install_shelter_rpm "$shelter_rpm"
    CA_SHELTER_BIN="$(command -v shelter || true)"
    [[ -n "$CA_SHELTER_BIN" ]] || die "Shelter RPM installed but shelter is not on PATH"
    export CA_SHELTER_BIN
  fi

  "$CA_SHELTER_BIN" --version >/dev/null
  log "using Shelter: $CA_SHELTER_BIN"
  if [[ "${CA_MODE:-}" != "cleanup" && "${CA_MODE:-}" != "cleanup-openclaw-vllm" && "${CA_MODE:-}" != "cleanup-vllm-inference" && "$CA_REFERENCE_VALUES" == "rekor" && ! -x "$CA_SLSA_GENERATOR" ]]; then
    die "SLSA generator is required for rekor mode: $CA_SLSA_GENERATOR"
  fi
}

resolve_shelter_rpm() {
  if [[ -n "${CA_SHELTER_RPM:-}" ]]; then
    [[ -f "$CA_SHELTER_RPM" ]] || die "Shelter RPM does not exist: $CA_SHELTER_RPM"
    printf '%s\n' "$CA_SHELTER_RPM"
    return
  fi

  local rpm
  for rpm in "$ROOT_DIR"/hack/shelter-*.rpm; do
    [[ -f "$rpm" ]] || continue
    printf '%s\n' "$rpm"
    return
  done
  die "Shelter is required and no bundled Shelter RPM was found under $ROOT_DIR/hack. Set CA_SHELTER_BIN or CA_SHELTER_RPM."
}

install_shelter_rpm() {
  local rpm="$1"
  local pm
  is_root || die "Shelter is missing and installing the Shelter RPM requires root. Re-run as root, install Shelter first, or set CA_SHELTER_BIN."
  pm="$(package_manager)" || die "yum or dnf is required to install the Shelter RPM"
  log "installing Shelter RPM: $rpm"
  "$pm" install -y "$rpm"
}

build_confidential_agent() {
  if [[ "$CA_SKIP_CARGO_BUILD" == "1" ]]; then
    [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
    [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
    [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary is missing: $CA_GATEWAY_BIN"
    if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
      [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary is missing: $CA_PEP_BIN"
    fi
    return
  fi
  ensure_rust_toolchain
  if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    log "building Confidential Agent host CLI, guest daemon and gateway"
    (cd "$ROOT_DIR" && cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway)
  else
    log "building Confidential Agent host CLI, guest daemon, gateway and PEP"
    (cd "$ROOT_DIR" && cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep)
  fi
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary was not built: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary was not built: $CA_AGENTD_BIN"
  [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary was not built: $CA_GATEWAY_BIN"
  if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
    [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary was not built: $CA_PEP_BIN"
  fi
}

install_confidential_agent_cli() {
  if [[ "${CA_INSTALL_CLI:-1}" != "1" ]]; then
    return 0
  fi
  is_root || die "installing confidential-agent to /usr/local/bin requires root"
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
  [[ -x "$CA_GATEWAY_BIN" ]] || die "cai-gateway binary is missing: $CA_GATEWAY_BIN"
  local cli_dest="/usr/local/bin/confidential-agent"
  local agentd_dest="/usr/local/bin/confidential-agentd"
  local gateway_dest="/usr/local/bin/cai-gateway"
  install -D -m 0755 "$CA_BIN" "$cli_dest"
  install -D -m 0755 "$CA_AGENTD_BIN" "$agentd_dest"
  install -D -m 0755 "$CA_GATEWAY_BIN" "$gateway_dest"
  CA_BIN="$cli_dest"
  CA_AGENTD_BIN="$agentd_dest"
  CA_GATEWAY_BIN="$gateway_dest"
  if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
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

build_tools_image() {
  ensure_docker_ready
  if [[ "$CA_REBUILD_TOOLS_IMAGE" != "1" ]] && docker image inspect "$CA_TOOLS_IMAGE" >/dev/null 2>&1; then
    log "tools image already exists: $CA_TOOLS_IMAGE"
    return
  fi
  log "building tools image: $CA_TOOLS_IMAGE"
  local build_args=()
  if [[ -n "${CA_TOOLS_IMAGE_BUILD_ARGS:-}" ]]; then
    # Whitespace-split simple docker build flags such as --network and --build-arg.
    read -r -a build_args <<<"$CA_TOOLS_IMAGE_BUILD_ARGS"
  fi
  local build_cmd=(docker build)
  if [[ ${#build_args[@]} -gt 0 ]]; then
    build_cmd+=("${build_args[@]}")
  fi
  build_cmd+=(--build-arg "GITHUB_PROXY_URL=${CA_GITHUB_PROXY_URL-https://gh-proxy.org/}")
  build_cmd+=(--build-arg "GITHUB_PROXY_FIRST=${CA_GITHUB_PROXY_FIRST-1}")
  build_cmd+=(--build-arg "GITHUB_DOWNLOAD_CONNECT_TIMEOUT=${CA_GITHUB_DOWNLOAD_CONNECT_TIMEOUT-20}")
  build_cmd+=(--build-arg "GITHUB_DOWNLOAD_MAX_TIME=${CA_GITHUB_DOWNLOAD_MAX_TIME-120}")
  build_cmd+=(--build-arg "GITHUB_DOWNLOAD_SPEED_LIMIT=${CA_GITHUB_DOWNLOAD_SPEED_LIMIT-65536}")
  build_cmd+=(--build-arg "GITHUB_DOWNLOAD_SPEED_TIME=${CA_GITHUB_DOWNLOAD_SPEED_TIME-20}")
  build_cmd+=(-t "$CA_TOOLS_IMAGE" -f tools/Dockerfile .)
  (cd "$ROOT_DIR" && "${build_cmd[@]}")
}

ca_cmd() {
  "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" "$@"
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
    if [[ "$CA_NON_INTERACTIVE" == "1" && "$CA_ASSUME_YES" != "1" ]]; then
      die "peering '$label' already exists with a different CIDR. Re-run with --yes to replace it."
    fi
    if [[ "$CA_ASSUME_YES" == "1" ]] || confirm "Replace operator peering '$label' with $cidr?" "y"; then
      ca_cmd peering remove "$label"
    else
      die "operator peering replacement was declined"
    fi
  fi
  log "adding operator peering '$label' for $cidr"
  ca_cmd peering add --role operator --cidr "$cidr" --label "$label" --note "$note"
}

ensure_operator_peering() {
  install -d -m 0700 "$CA_WORK_DIR"
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
    ensure_operator_peering_entry "$label" "$cidr" "operator access CIDR selected by one-click installer"
  done
  if [[ -n "${CA_DEPLOYER_CIDR:-}" ]] \
    && ! cidr_list_contains "$allowed_cidrs" "$CA_DEPLOYER_CIDR" \
    && ! cidr_list_contains "$allowed_cidrs" "0.0.0.0/0"; then
    ensure_operator_peering_entry deployer "$CA_DEPLOYER_CIDR" "deployment host egress CIDR detected by one-click installer"
  fi
}

build_openclaw_image() {
  if [[ "$CA_SKIP_BUILD" == "1" ]]; then
    log "skipping $CA_SERVICE_LABEL image build"
    return
  fi
  local phase
  phase="$(openclaw_phase || true)"
  if [[ "$phase" == "deployed" || "$phase" == "active" ]]; then
    log "$CA_SERVICE_LABEL is $phase; skipping image build (existing deployment detected)"
    return
  fi
  log "building $CA_SERVICE_LABEL confidential image"
  (cd "$CA_PROJECT_DIR" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" build --spec "$CA_SPEC_PATH")
}

openclaw_phase() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3.11 - "$status_json" "$CA_SERVICE_ID" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
service_id = sys.argv[2]
for item in items:
    if item.get("service_id") == service_id:
        phase = item.get("phase")
        if phase:
            print(phase)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

openclaw_is_active() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3.11 - "$status_json" "$CA_SERVICE_ID" <<'PY' >/dev/null 2>&1
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
service_id = sys.argv[2]
for item in items:
    if item.get("service_id") == service_id and item.get("phase") == "active":
        cloud = item.get("cloud") if isinstance(item.get("cloud"), dict) else {}
        if cloud.get("present") is True:
            raise SystemExit(0)
raise SystemExit(1)
PY
}

openclaw_public_ip() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3.11 - "$status_json" "$CA_SERVICE_ID" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
service_id = sys.argv[2]
for item in items:
    if item.get("service_id") == service_id:
        cloud = item.get("cloud") if isinstance(item.get("cloud"), dict) else {}
        public_ip = cloud.get("public_ip")
        if public_ip:
            print(public_ip)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

openclaw_debug_ssh_key() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3.11 - "$status_json" "$CA_SERVICE_ID" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
service_id = sys.argv[2]
for item in items:
    if item.get("service_id") == service_id:
        build = item.get("build") if isinstance(item.get("build"), dict) else {}
        debug_ssh = build.get("debug_ssh") if isinstance(build.get("debug_ssh"), dict) else {}
        private_key = debug_ssh.get("private_key")
        if private_key:
            print(private_key)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

deploy_openclaw_service() {
  if [[ "$CA_SKIP_DEPLOY" == "1" ]]; then
    log "skipping $CA_SERVICE_LABEL deploy"
    return
  fi
  if openclaw_is_active; then
    log "$CA_SERVICE_LABEL is already active; skipping cloud deploy"
    CA_DEPLOY_SKIPPED_ACTIVE=1
    return
  fi
  local phase
  phase="$(openclaw_phase || true)"
  if [[ "$phase" == "deployed" ]]; then
    local public_ip
    public_ip="$(openclaw_public_ip)" || die "$CA_SERVICE_LABEL is deployed but has no recorded public IP; run 'confidential-agent destroy $CA_SERVICE_ID' and redeploy"
    log "$CA_SERVICE_LABEL is deployed but not active; resuming from resource injection to $public_ip"
    (cd "$CA_PROJECT_DIR" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" inject --spec "$CA_SPEC_PATH" --target-ip "$public_ip")
    return
  fi
  log "deploying $CA_SERVICE_LABEL to Aliyun"
  (cd "$CA_PROJECT_DIR" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" deploy --spec "$CA_SPEC_PATH")
}

sync_active_openclaw_resources() {
  if [[ "${CA_DEPLOY_SKIPPED_ACTIVE:-0}" != "1" || "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  local public_ip
  public_ip="$(openclaw_public_ip)" || {
    warn "could not determine active $CA_SERVICE_LABEL public IP; skipping resource sync"
    return
  }
  log "syncing resources to active $CA_SERVICE_LABEL at $public_ip"
  (cd "$CA_PROJECT_DIR" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" inject --spec "$CA_SPEC_PATH" --target-ip "$public_ip")
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
  public_ip="$(openclaw_public_ip)" || {
    warn "could not determine active OpenClaw public IP; skipping gateway restart"
    return
  }
  key="$(openclaw_debug_ssh_key)" || {
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
    if ca_cmd status --live --json >"$status_json" 2>"$CA_WORK_DIR/status-live.err"; then
      if python3.11 - "$status_json" "$CA_SERVICE_ID" <<'PY' >/dev/null 2>&1
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
service_id_filter = sys.argv[2]
for item in items:
    local = item.get("local") if isinstance(item.get("local"), dict) else {}
    daemon = item.get("daemon") if isinstance(item.get("daemon"), dict) else {}
    service_id = item.get("service_id") or item.get("service") or local.get("service_id") or daemon.get("service_id")
    if service_id == service_id_filter:
        if daemon.get("app_ready") is True:
            raise SystemExit(0)
        if daemon.get("phase") in ("running", "ready", "active"):
            raise SystemExit(0)
        if item.get("phase") in ("ready", "active") or local.get("phase") in ("ready", "active"):
            raise SystemExit(0)
raise SystemExit(1)
PY
      then
        log "live status is available"
        return
      fi
    fi
    sleep 10
  done
  warn "live status did not become ready within ${CA_STATUS_TIMEOUT_SEC}s; see $CA_WORK_DIR/status-live.err"
}

parse_connect_port() {
  awk '/^connect 127\.0\.0\.1:/ { split($2, a, ":"); print a[2]; exit }' "$1"
}

start_connect() {
  if [[ "$CA_SKIP_DEPLOY" == "1" || "$CA_START_CONNECT" != "1" ]]; then
    return
  fi
  require_cmd setsid
  local log_path="$CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.log"
  local pid_path="$CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.pid"
  log "starting local RATS-TLS connect tunnel"
  if [[ -f "$pid_path" ]]; then
    local old_pid
    old_pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$old_pid" ]] && kill -0 "$old_pid" >/dev/null 2>&1; then
      log "stopping previous connect tunnel (pid $old_pid)"
      kill "$old_pid" >/dev/null 2>&1 || true
      sleep 1
    fi
    rm -f "$pid_path"
  fi
  rm -f "$log_path" "$pid_path"
  setsid "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" connect --service "$CA_SERVICE_ID" >"$log_path" 2>&1 &
  printf '%s\n' "$!" >"$pid_path"

  local deadline=$((SECONDS + CA_CONNECT_TIMEOUT_SEC))
  local port=""
  local readiness_path="${CA_CONNECT_READINESS_PATH:-/openclaw}"
  while ((SECONDS < deadline)); do
    port="$(parse_connect_port "$log_path" || true)"
    if [[ -n "$port" ]] && curl -fsSL --max-time 5 "http://127.0.0.1:$port$readiness_path" >/dev/null 2>&1; then
      CA_CONNECT_PORT="$port"
      if [[ "$CA_SERVICE_ID" == "vllm-inference" ]]; then
        log "vLLM is reachable through connect: http://127.0.0.1:$port/v1/models"
      else
        log "OpenClaw Web is reachable through connect: http://127.0.0.1:$port/openclaw"
      fi
      return
    fi
    if ! kill -0 "$(cat "$pid_path")" >/dev/null 2>&1; then
      if [[ "$CA_SERVICE_ID" == "vllm-inference" ]]; then
        warn "connect process exited before vLLM became reachable; see $log_path"
      else
        warn "connect process exited before OpenClaw became reachable; see $log_path"
      fi
      return
    fi
    sleep 3
  done
  if [[ "$CA_SERVICE_ID" == "vllm-inference" ]]; then
    warn "connect did not become HTTP-ready (probe $readiness_path) within ${CA_CONNECT_TIMEOUT_SEC}s; see $log_path"
  else
    warn "connect did not become HTTP-ready within ${CA_CONNECT_TIMEOUT_SEC}s; see $log_path"
  fi
}

run_web_smoke() {
  if [[ -z "${CA_CONNECT_PORT:-}" ]]; then
    return
  fi
  local body="$CA_WORK_DIR/web-smoke.html"
  log "verifying OpenClaw control UI through connect"
  if ! curl -fsSL --max-time 15 "http://127.0.0.1:$CA_CONNECT_PORT/openclaw" -o "$body"; then
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
  local pid_path="$CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.pid"
  [[ -f "$pid_path" ]] || return 0
  local pid
  pid="$(cat "$pid_path" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
    log "stopping local connect tunnel (pid $pid)"
    kill "$pid" >/dev/null 2>&1 || true
  fi
  rm -f "$pid_path"
  CA_CONNECT_PORT=""
}

print_summary() {
  local connect_pid=""
  local pid_path="$CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.pid"
  [[ -f "$pid_path" ]] && connect_pid="$(cat "$pid_path" 2>/dev/null || true)"
  cat <<EOF

Confidential Agent one-click summary
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
  vllm:      curl http://127.0.0.1:$CA_CONNECT_PORT/v1/models
  chat:      curl http://127.0.0.1:$CA_CONNECT_PORT/v1/chat/completions \\
                 -H 'Content-Type: application/json' \\
                 -d '{"model":"$CA_VLLM_INFERENCE_SERVED_MODEL_NAME","messages":[{"role":"user","content":"hi"}]}'
  connect:   running (pid $connect_pid, log $CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.log)
EOF
    else
      cat <<EOF
  web:       http://127.0.0.1:$CA_CONNECT_PORT/openclaw
  ws/api:    ws://127.0.0.1:$CA_CONNECT_PORT
  tui:       openclaw tui --url ws://127.0.0.1:$CA_CONNECT_PORT --token "$CA_GATEWAY_TOKEN"
  connect:   running (pid $connect_pid, log $CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.log)
EOF
    fi
  elif [[ -n "$connect_pid" ]] && kill -0 "$connect_pid" >/dev/null 2>&1; then
    cat <<EOF
  connect:   tunnel running (pid $connect_pid); probe ${CA_CONNECT_READINESS_PATH:-/openclaw} at http://127.0.0.1:<port>
             log: $CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.log
EOF
  else
    cat <<EOF
  connect:   run this when the service is active:
             confidential-agent connect --service $CA_SERVICE_ID
EOF
  fi
  if [[ -n "$connect_pid" ]]; then
    cat <<EOF

Cleanup:
  confidential-agent destroy $CA_SERVICE_ID
  kill $connect_pid  # only if connect is still running and you want to stop the local tunnel

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
  build_tools_image
  log "install-only completed"
}

run_deploy_openclaw() {
  install_os_dependencies
  ensure_host_openclaw_runtime
  ensure_aliyun_credentials
  ensure_bailian_credentials
  ensure_dingtalk_credentials
  resolve_allowed_cidr
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  build_tools_image
  prepare_openclaw_specs
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

run_deploy_openclaw_vllm() {
  install_os_dependencies
  ensure_host_openclaw_runtime
  ensure_aliyun_credentials
  ensure_dingtalk_credentials
  resolve_allowed_cidr
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  build_tools_image
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
  ensure_aliyun_credentials
  resolve_allowed_cidr
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  build_tools_image
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
  ensure_aliyun_credentials
  ensure_shelter
  build_confidential_agent
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
