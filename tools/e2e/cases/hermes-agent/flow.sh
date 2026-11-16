#!/usr/bin/env bash

HERMES_HOME=/opt/data
HERMES_INSTALL_DIR=/usr/local/lib/hermes-agent

collect_hermes_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/hermes-guest-diagnostics-$label.txt"
  record_cmd "ssh hermes '<mkosi runtime diagnostics>'"
  ssh_guest "$key" "$host" /bin/sh >"$out" 2>&1 <<'REMOTE' || true
set +e
SERVICE=cai-hermes-agent.service
DATA=/opt/data
echo "### Hermes service"
systemctl status "$SERVICE" --no-pager -l
echo "### Hermes journal"
journalctl -u "$SERVICE" -n 300 --no-pager
echo "### Hermes launcher"
sed -n '1,260p' /usr/local/bin/cai-hermes-agent 2>&1
echo "### Hermes unit"
sed -n '1,160p' /etc/systemd/system/cai-hermes-agent.service 2>&1
echo "### Hermes install"
cat /usr/local/share/confidential-agent/hermes-install.txt 2>&1
ls -la /usr/local/lib/hermes-agent 2>&1 | head -80
/usr/local/bin/hermes --version 2>&1
echo "### injected data metadata"
find "$DATA" -maxdepth 2 -printf "%p %m %u:%g %s bytes\n" 2>&1
echo "### resource keys"
printf "env_keys="
cut -d= -f1 "$DATA/.env" 2>/dev/null | paste -sd, -
printf "\nconfig_head:\n"
sed -n "1,40p" "$DATA/config.yaml" 2>&1
echo "### processes"
ps -ef | grep -E '[h]ermes|[c]ai-hermes' 2>&1
echo "### local HTTP"
curl -fsS -D - --max-time 10 http://127.0.0.1:8642/health 2>&1
REMOTE
  record_file_as_block "Hermes guest diagnostics ($label):" "$out" text
}

assert_hermes_guest_runtime() {
  local host="$1"
  local key="$2"
  local out="$WORK_DIR/hermes-guest-runtime-assertions.txt"
  local err="$WORK_DIR/hermes-guest-runtime-assertions.err"
  record_cmd "ssh hermes '<mkosi runtime assertions>'"
  if ssh_guest "$key" "$host" /bin/sh >"$out" 2>"$err" <<'REMOTE'
set -eu

SERVICE=cai-hermes-agent.service
DATA=/opt/data

check_eq() {
  actual="$1"
  expected="$2"
  label="$3"
  if [ "$actual" != "$expected" ]; then
    printf '%s: expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf '%s=%s\n' "$label" "$actual"
}

check_file() {
  path="$1"
  [ -s "$path" ] || {
    printf 'missing or empty file: %s\n' "$path" >&2
    exit 1
  }
}

systemctl is-active --quiet "$SERVICE"
printf 'service=%s\n' "$(systemctl is-active "$SERVICE")"

check_file /usr/local/bin/cai-hermes-agent
check_file /usr/local/bin/hermes
check_file /usr/local/share/confidential-agent/hermes-install.txt
test -d /usr/local/lib/hermes-agent/.git
runuser -u hermes -- env HERMES_HOME="$DATA" /usr/local/bin/hermes --version
check_file "$DATA/.env"
check_file "$DATA/config.yaml"

id -u hermes >/dev/null
check_eq "$(id -u hermes)" "10000" "uid:hermes"
check_eq "$(id -g hermes)" "10000" "gid:hermes"
check_eq "$(stat -c %a "$DATA/.env")" "600" "mode:$DATA/.env"
check_eq "$(stat -c %a "$DATA/config.yaml")" "600" "mode:$DATA/config.yaml"
check_eq "$(stat -c %u:%g "$DATA/.env")" "10000:10000" "owner:$DATA/.env"
check_eq "$(stat -c %u:%g "$DATA/config.yaml")" "10000:10000" "owner:$DATA/config.yaml"

grep -Fqx 'API_SERVER_ENABLED=true' "$DATA/.env"
grep -Fqx 'API_SERVER_HOST=0.0.0.0' "$DATA/.env"
grep -Fqx 'API_SERVER_PORT=8642' "$DATA/.env"
grep -Fqx 'HERMES_HOME=/opt/data' "$DATA/.env"
grep -Fq 'model:' "$DATA/config.yaml"
grep -Fq 'provider: alibaba' "$DATA/config.yaml"

grep -Fq '/usr/local/bin/hermes gateway run' /usr/local/bin/cai-hermes-agent
grep -Fq 'runuser -u "$HERMES_USER"' /usr/local/bin/cai-hermes-agent
grep -Fq 'branch=' /usr/local/share/confidential-agent/hermes-install.txt
grep -Fq 'commit=' /usr/local/share/confidential-agent/hermes-install.txt

test ! -e /usr/local/libexec/confidential-agent/patch-hermes-launcher.sh
test ! -e /usr/local/libexec/confidential-agent/hermes/install-hermes-rootfs.sh
test ! -d /opt/confidential-agent/hermes/rootfs
if systemctl list-unit-files 'shelter-container-hermes-agent.service' --no-legend 2>/dev/null | grep -q 'shelter-container-hermes-agent.service'; then
  printf 'unexpected Shelter container Hermes unit is installed\n' >&2
  exit 1
fi
if command -v podman >/dev/null 2>&1 && podman ps --format '{{.Names}}' 2>/dev/null | grep -qx 'hermes-agent'; then
  printf 'unexpected Hermes podman runtime container is running\n' >&2
  exit 1
fi

http_code="$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 3 --max-time 5 http://127.0.0.1:8642/health)"
check_eq "$http_code" "200" "gateway_health_http_code"
REMOTE
  then
    record_file_as_block "Hermes guest runtime assertions:" "$out" text
    record_file_as_block "Hermes guest runtime assertion stderr:" "$err" text
    return 0
  fi
  record_file_as_block "Hermes guest runtime assertions:" "$out" text
  record_file_as_block "Hermes guest runtime assertion stderr:" "$err" text
  return 1
}

wait_for_hermes_http_ready() {
  local host="$1"
  local key="$2"
  local timeout_s="$3"
  local out="$WORK_DIR/hermes-http-ready.txt"
  local err="$WORK_DIR/hermes-http-ready.err"
  local attempt_out="$WORK_DIR/hermes-http-ready.attempt.out"
  local attempt_err="$WORK_DIR/hermes-http-ready.attempt.err"
  local deadline=$((SECONDS + timeout_s))
  local status=0
  record_cmd "ssh hermes '<Hermes local HTTP readiness>'"
  : >"$out"
  : >"$err"
  while (( SECONDS < deadline )); do
    status=0
    timeout 30s ssh -i "$key" \
      -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=10 \
      -o BatchMode=yes \
      root@"$host" /bin/sh >"$attempt_out" 2>"$attempt_err" <<'REMOTE' || status=$?
set -eu

SERVICE=cai-hermes-agent.service
if curl -fsS --connect-timeout 3 --max-time 10 http://127.0.0.1:8642/health; then
  systemctl is-active --quiet "$SERVICE"
  exit 0
fi
if systemctl is-failed --quiet "$SERVICE"; then
  printf 'Hermes service failed while waiting for local HTTP readiness\n' >&2
  systemctl status "$SERVICE" --no-pager -l >&2 || true
  journalctl -b -u "$SERVICE" -n 160 --no-pager >&2 || true
  exit 2
fi
exit 3
REMOTE
    cat "$attempt_out" >>"$out"
    cat "$attempt_err" >>"$err"
    if [[ "$status" == "0" ]]; then
      rm -f "$attempt_out" "$attempt_err"
      record_file_as_block "Hermes local HTTP readiness:" "$out" text
      record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
      return 0
    fi
    if [[ "$status" == "2" ]]; then
      rm -f "$attempt_out" "$attempt_err"
      record_file_as_block "Hermes local HTTP readiness:" "$out" text
      record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
      return 1
    fi
    sleep 5
  done

  printf 'Timed out waiting for Hermes local HTTP readiness\n' >>"$err"
  ssh_guest "$key" "$host" 'journalctl -b -u cai-hermes-agent.service -n 200 --no-pager' >>"$err" 2>&1 || true
  rm -f "$attempt_out" "$attempt_err"
  record_file_as_block "Hermes local HTTP readiness:" "$out" text
  record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
  return 1
}

run_hermes_chat_probe() {
  local url="$1"
  local token="$2"
  local model="$3"
  local message="$4"
  local expect="$5"
  local output="$6"
  local timeout_ms="$7"
  local stderr="${output}.err"
  record_cmd "node tools/e2e/probes/hermes-chat-probe.mjs --url $url --token '<redacted>' --model $model --message '<redacted>' --expect $expect"
  if node "$ROOT_DIR/tools/e2e/probes/hermes-chat-probe.mjs" \
    --url "$url" \
    --token "$token" \
    --model "$model" \
    --message "$message" \
    --expect "$expect" \
    --timeout-ms "$timeout_ms" >"$output" 2>"$stderr"; then
    cat "$output"
    record_file_as_block "Hermes chat probe:" "$output" json
    record_file_as_block "Hermes chat probe stderr:" "$stderr" text
    return 0
  fi
  cat "$output"
  cat "$stderr" >&2
  record_file_as_block "Hermes chat probe:" "$output" json
  record_file_as_block "Hermes chat probe stderr:" "$stderr" text
  return 1
}

hermes_ssh_info() {
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

assert_hermes_secret_rendering() {
  local spec="$HERMES_DIR/hermes-agent.yaml"
  local dashscope_key="$1"
  for secret in "$HERMES_API_SERVER_KEY" "$dashscope_key"; do
    if [[ -n "$secret" ]] && grep -Fq "$secret" "$spec"; then
      echo "Hermes rendered spec contains secret material" >&2
      return 1
    fi
  done

  grep -Fq "app_service: cai-hermes-agent.service" "$spec"
  grep -Fq "packages: [ca-certificates, curl, findutils, git, libatomic, python3.11, shadow-utils, tar, util-linux, xz]" "$spec"
  grep -Fq "source: ./files/install-hermes-agent-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/hermes/install-hermes-agent-runtime.sh" "$spec"
  grep -Fq "source: ./files/cai-hermes-agent" "$spec"
  grep -Fq "target: /usr/local/bin/cai-hermes-agent" "$spec"
  grep -Fq "target: /etc/systemd/system/cai-hermes-agent.service" "$spec"
  grep -Fq "scripts: [./install-hermes-agent.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: $HERMES_HOME/.env" "$spec"
  grep -Fq "target: $HERMES_HOME/config.yaml" "$spec"
  grep -Fq 'mutable: true' "$spec"
  grep -Fq 'owner: "10000"' "$spec"
  grep -Fq 'group: "10000"' "$spec"
  grep -Fq 'mode: "0600"' "$spec"

  if grep -Eq 'container:|shelter-container-hermes-agent|patch-hermes-launcher|/var/lib/confidential-agent/hermes-agent/data|/opt/confidential-agent/hermes/rootfs|podman|oci-archive|HERMES_IMAGE' "$spec"; then
    echo "Hermes rendered spec still contains container/rootfs wiring" >&2
    return 1
  fi

  test -s "$HERMES_DIR/secrets/hermes.env"
  test -s "$HERMES_DIR/secrets/config.yaml"
  test -x "$HERMES_DIR/install-hermes-agent.sh"
  test -x "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  test -x "$HERMES_DIR/files/cai-hermes-agent"
  test -s "$HERMES_DIR/files/cai-hermes-agent.service"
  grep -Fq 'export HERMES_BRANCH=' "$HERMES_DIR/install-hermes-agent.sh"
  grep -Fq 'export HERMES_COMMIT=' "$HERMES_DIR/install-hermes-agent.sh"
  assert_init_script_extends_example \
    "$HERMES_DIR/install-hermes-agent.sh" \
    "$ROOT_DIR/examples/hermes-agent/install-hermes-agent.sh" \
    HERMES_BRANCH HERMES_COMMIT
  grep -Fq 'CA_GITHUB_PROXY_URL:-https://gh-proxy.org/' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq 'HERMES_PYPI_INDEX_URL' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq 'HERMES_NPM_REGISTRY_URL' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq 'https://github.com/NousResearch/hermes-agent.git' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq -- '--skip-browser' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq -- '--skip-setup' "$HERMES_DIR/files/install-hermes-agent-runtime.sh"
  grep -Fq '/usr/local/bin/hermes gateway run' "$HERMES_DIR/files/cai-hermes-agent"
  if grep -Eq 'podman|docker|oci-archive|chroot|HERMES_IMAGE|rootfs' "$HERMES_DIR/files/install-hermes-agent-runtime.sh" "$HERMES_DIR/files/cai-hermes-agent"; then
    echo "Hermes init output still contains container/rootfs implementation details" >&2
    return 1
  fi
  record "- Hermes init output matches the mkosi source-install example and does not contain provider/API tokens."
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_HERMES_DISK_GB:-30}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/hermes-agent-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  HERMES_DIR="$INIT_OUTPUT_DIR/hermes-agent"
  HERMES_BRANCH="${E2E_HERMES_BRANCH:-main}"
  HERMES_COMMIT="${E2E_HERMES_COMMIT:-}"
  HERMES_API_SERVER_KEY="${E2E_HERMES_API_SERVER_KEY:-$(openssl rand -hex 32)}"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  HERMES_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
  GATEWAY_READY_TIMEOUT="${E2E_HERMES_GATEWAY_READY_TIMEOUT:-900}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-Please respond with CA_E2E_OK and no other text.}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"

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

  init_step_log "Confidential Agent Hermes mkosi E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway

  local allowed_cidr cosign_key dashscope_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  export COSIGN_KEY="$cosign_key"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB HERMES_BRANCH HERMES_COMMIT HERMES_API_SERVER_KEY DASHSCOPE_BASE_URL HERMES_MODEL HERMES_HOME

  mapfile -d '' init_args < <(init_common_args "$INIT_OUTPUT_DIR" "$DISK_GB")
  init_args+=(
    --dashscope-api-key "$dashscope_key"
    --dashscope-base-url "$DASHSCOPE_BASE_URL"
    --model "$HERMES_MODEL"
    --hermes-branch "$HERMES_BRANCH"
    --hermes-api-server-key "$HERMES_API_SERVER_KEY"
  )
  if [[ -n "$HERMES_COMMIT" ]]; then
    init_args+=(--hermes-commit "$HERMES_COMMIT")
  fi
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/init.out" "$WORK_DIR/init.err" hermes "${init_args[@]}"; then
    record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
    record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
    return 1
  fi
  record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
  record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
  assert_hermes_secret_rendering "$dashscope_key"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Hermes source branch: \`$HERMES_BRANCH\`."
  if [[ -n "$HERMES_COMMIT" ]]; then
    record "- Hermes source commit: \`$HERMES_COMMIT\`."
  fi
  record "- Hermes home: \`$HERMES_HOME\`."
  record "- Hermes API server key generated but not printed."

  validate_specs "$STATE_DIR" "$HERMES_DIR/hermes-agent.yaml"

  register_destroy_target "$STATE_DIR" hermes-agent

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$HERMES_DIR/hermes-agent.yaml"
  fi
  record_manifest_variants "$STATE_DIR" hermes-agent

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    ca_run "$STATE_DIR" deploy --spec "$HERMES_DIR/hermes-agent.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" hermes-agent 1200
  ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  mapfile -t ssh_lines < <(hermes_ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  wait_for_hermes_http_ready "$host" "$key" "$GATEWAY_READY_TIMEOUT"
  assert_hermes_guest_runtime "$host" "$key"

  local connect_port
  connect_port="$(start_connect_until_local_port_ready "$STATE_DIR" hermes-agent --service hermes-agent)"
  record "Connect mapped Hermes to \`127.0.0.1:$connect_port\`."

  if ! run_hermes_chat_probe \
      "http://127.0.0.1:$connect_port" \
      "$HERMES_API_SERVER_KEY" \
      "$HERMES_MODEL" \
      "$CHAT_MESSAGE" \
      "$CHAT_EXPECT" \
      "$WORK_DIR/hermes-chat-probe.json" \
      "$CHAT_TIMEOUT_MS"; then
    collect_hermes_guest_diagnostics "$host" "$key" "probe-failure"
    return 1
  fi
}
