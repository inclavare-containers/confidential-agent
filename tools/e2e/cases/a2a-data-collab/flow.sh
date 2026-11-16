#!/usr/bin/env bash

data_collab_signing_enabled() {
  case "${E2E_A2A_SIGNING:-${A2A_SIGNING:-0}}" in
    1 | true | TRUE | yes | YES | y | Y | on | ON | signed | sigstore | keyless) return 0 ;;
    *) return 1 ;;
  esac
}

resolve_data_collab_signer_pin() {
  local issuer="${A2A_SIGNER_ISSUER:-${E2E_A2A_SIGNER_ISSUER:-}}"
  local subject="${A2A_SIGNER_SUBJECT:-${E2E_A2A_SIGNER_SUBJECT:-}}"

  if [[ (-z "$issuer" || -z "$subject") && -n "${CA_A2A_SIGSTORE_IDENTITY_TOKEN:-}" ]]; then
    local decoded
    decoded="$(
      python3.11 <<'PY'
import base64
import json
import os
import sys

token = os.environ.get("CA_A2A_SIGSTORE_IDENTITY_TOKEN", "").strip()
parts = token.split(".")
if len(parts) < 2:
    sys.exit("CA_A2A_SIGSTORE_IDENTITY_TOKEN must be a JWT")
payload = parts[1] + "=" * (-len(parts[1]) % 4)
claims = json.loads(base64.urlsafe_b64decode(payload.encode("ascii")).decode("utf-8"))
print(claims.get("iss", ""))
print(claims.get("sub", ""))
PY
    )"
    issuer="${issuer:-$(printf '%s\n' "$decoded" | sed -n '1p')}"
    subject="${subject:-$(printf '%s\n' "$decoded" | sed -n '2p')}"
  fi

  if [[ -z "$issuer" || -z "$subject" ]]; then
    echo "a2a-data-collab requires A2A_SIGNER_ISSUER/A2A_SIGNER_SUBJECT, E2E_A2A_SIGNER_*, or a JWT CA_A2A_SIGSTORE_IDENTITY_TOKEN with iss/sub claims." >&2
    exit 2
  fi
  if [[ -z "${CA_A2A_SIGSTORE_IDENTITY_TOKEN:-}" && -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" && "${E2E_ALLOW_INTERACTIVE_SIGSTORE:-0}" != "1" ]]; then
    echo "a2a-data-collab signs AgentCards with cosign keyless; set CA_A2A_SIGSTORE_IDENTITY_TOKEN or run in a CI environment with OIDC token request envs." >&2
    exit 2
  fi

  export A2A_SIGNER_ISSUER="$issuer"
  export A2A_SIGNER_SUBJECT="$subject"
}

wait_for_a2a_peer_state() {
  local service_ip="$1"
  local peer="$2"
  local expected="$3"
  local error_pattern="${4:-}"
  local timeout="${5:-300}"
  local deadline=$((SECONDS + timeout))
  local path="$WORK_DIR/a2a-status-$peer-$expected.json"
  while (( SECONDS < deadline )); do
    if curl -fsS --max-time 5 "http://$service_ip:8088/status" -o "$path"; then
      if python3.11 - "$path" "$peer" "$expected" "$error_pattern" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    status = json.load(f)
peer = status.get("a2a_peers", {}).get(sys.argv[2])
if not peer or peer.get("state") != sys.argv[3]:
    raise SystemExit(1)
pattern = sys.argv[4]
if pattern and not re.search(pattern, peer.get("error") or "", re.I):
    raise SystemExit(1)
PY
      then
        record_file_as_block "A2A peer $peer reached state $expected:" "$path" json
        return 0
      fi
    fi
    sleep 5
  done
  record_file_as_block "Last A2A status for $peer:" "$path" json
  echo "timed out waiting for A2A peer '$peer' state '$expected'" >&2
  return 1
}

assert_agent_card_signed() {
  local path="$1"
  python3.11 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    card = json.load(f)
signatures = card.get("signatures") or []
if not signatures:
    raise SystemExit(f"{sys.argv[1]} has no AgentCard signatures")
PY
}

assert_agent_card_unsigned() {
  local path="$1"
  python3.11 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    card = json.load(f)
signatures = card.get("signatures") or []
if signatures:
    raise SystemExit(f"{sys.argv[1]} unexpectedly has AgentCard signatures")
PY
}

fetch_data_collab_guest_diagnostics() {
  local label="$1"
  local host="$2"
  local key="$3"
  local output="$WORK_DIR/a2a-data-collab-$label-diagnostics.txt"

  record_cmd "ssh -i <debug_ssh> root@$host '<a2a data collaboration diagnostics>'"
  ssh_guest "$key" "$host" "set +e
echo '--- health ---'
curl -fsS http://127.0.0.1:18789/health || true
echo
echo '--- service status ---'
systemctl --no-pager -l status cai-a2a-llm-agent.service || true
echo
echo '--- service journal ---'
journalctl -u cai-a2a-llm-agent.service --no-pager -n 200 || true
echo
echo '--- platform services ---'
systemctl --no-pager -l status trusted-network-gateway.service cai-gateway.service confidential-agentd.service || true
echo
echo '--- platform journal ---'
journalctl -u trusted-network-gateway.service -u cai-gateway.service -u confidential-agentd.service --no-pager -n 300 || true
echo
echo '--- service directory ---'
cat /etc/cai/service-directory.json 2>/dev/null || true
echo
echo '--- tng config ---'
cat /etc/tng/config.json 2>/dev/null || true
echo
echo '--- gateway config redacted ---'
python3.11 - <<'PY' || true
import json
from pathlib import Path
path = Path('/etc/cai/gateway.json')
if path.exists():
    data = json.loads(path.read_text())
    if isinstance(data.get('identity'), dict):
        data['identity']['private_key'] = '<redacted>'
    print(json.dumps(data, indent=2, sort_keys=True))
PY
echo
echo '--- listening tcp sockets ---'
ss -ltnp || true
echo
echo '--- nat rules ---'
iptables -t nat -S 2>/dev/null | grep -E 'TNG|CAI|18789|18000|39000|39400' || true
echo
echo '--- audit tail ---'
tail -n 200 /var/log/cai-a2a-data-collab.jsonl 2>/dev/null || true" >"$output" 2>&1 || true
  record_file_as_block "$label A2A data collaboration guest diagnostics:" "$output" text
}

assert_a2a_guest_tng_28_config() {
  local label="$1"
  local host="$2"
  local key="$3"
  local output="$WORK_DIR/a2a-tng-$label.txt"
  local check

  wait_for_ssh "$host" "$key" 300
  check="set -euo pipefail; rpm_nevra=\$(rpm -q trusted-network-gateway); test \"\$rpm_nevra\" = trusted-network-gateway-2.9.2-1.al8.x86_64; tng_version=\$(/usr/bin/tng --version | sed -n '1p'); test \"\$tng_version\" = 'tng 2.9.2'; jq -e '([(.add_ingress // [])[], (.add_egress // [])[]]) as \$routes | (\$routes | length > 0) and (\$routes | all(.rats_tls.multiplex == true)) and ([\$routes[] | select(.verify? != null)] | length > 0) and ([\$routes[] | select(.verify? != null)] | all(.verify.as_type == \"builtin\" and .verify.attestation_policy.type == \"path\" and ((.verify | has(\"policy\")) | not) and ((.verify | has(\"policy_ids\")) | not)))' /etc/tng/config.json >/dev/null; printf 'tng_rpm=%s\\ntng_version=%s\\ntng_config_schema=2.9.2\\n' \"\$rpm_nevra\" \"\$tng_version\""

  record_cmd "ssh $label '<assert TNG 2.8 A2A config>'"
  ssh_guest "$key" "$host" "$check" >"$output"
  record_file_as_block "$label TNG 2.8 A2A config:" "$output" text
}

assert_a2a_connect_boundary() {
  local label="$1"
  local host="$2"
  local key="$3"
  local peer="$4"
  local output="$WORK_DIR/a2a-data-collab-$label-connect-boundary.json"
  local remote_peer

  wait_for_ssh "$host" "$key" 300
  record_cmd "ssh -i <debug_ssh> root@$host '<assert A2A peer uses connect semantics>'"
  remote_peer="$(printf '%q' "$peer")"
  ssh_guest "$key" "$host" "CAI_PEER=$remote_peer node -e '
const fs = require(\"fs\");

const peer = process.env.CAI_PEER;
const directory = JSON.parse(fs.readFileSync(\"/etc/cai/service-directory.json\", \"utf8\"));
const ports = ((directory.services || {})[peer] || {}).ports || [];
if (ports.length === 0) {
  throw new Error(\"missing service-directory ports for peer \" + peer);
}
const badPorts = ports.filter((port) => port.mode !== \"connect\" || port.protocol != null);
if (badPorts.length > 0) {
  throw new Error(\"A2A peer \" + peer + \" did not use pure connect ports: \" + JSON.stringify(badPorts));
}

let gateway = {};
if (fs.existsSync(\"/etc/cai/gateway.json\")) {
  gateway = JSON.parse(fs.readFileSync(\"/etc/cai/gateway.json\", \"utf8\"));
}

console.log(JSON.stringify({
  peer,
  ports,
  gateway_client_routes: (gateway.client_routes || []).length,
  gateway_server_routes: (gateway.server_routes || []).length,
}, null, 2));
' " >"$output"
  record_file_as_block "$label A2A connect boundary:" "$output" json
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/a2a-data-collab-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  ANALYST_DIR="$WORK_DIR/analyst-org"
  DATA_OWNER_DIR="$WORK_DIR/data-owner-org"
  ANALYST_STATE_DIR="$ANALYST_DIR/state"
  DATA_OWNER_STATE_DIR="$DATA_OWNER_DIR/state"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-300000}"
  TASK_MESSAGE="${E2E_A2A_DATA_TASK:-Assess AlphaCorp supply-chain risk. Use only aggregate data from the data owner and do not expose raw customer records or order IDs.}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd node
  require_cmd python3.11
  require_cmd ssh
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials
  if data_collab_signing_enabled; then
    resolve_data_collab_signer_pin
    export E2E_A2A_SIGNING=1
  else
    export E2E_A2A_SIGNING=0
  fi

  init_step_log "Confidential Agent A2A Data Collaboration E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway

  local dashscope_key allowed_cidr cosign_key
  dashscope_key="$(resolve_dashscope_key)"
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  export DASHSCOPE_KEY="$dashscope_key"
  export DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  export DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
  export COSIGN_KEY="$cosign_key"
  export INSTANCE_TYPE

  render_case
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- DashScope API key loaded but not printed."
  if data_collab_signing_enabled; then
    record "- AgentCard signing mode: \`sigstore-keyless\`"
    record "- AgentCard signer issuer: \`$A2A_SIGNER_ISSUER\`"
    record "- AgentCard signer subject: \`$A2A_SIGNER_SUBJECT\`"
  else
    record "- AgentCard signing mode: \`unsigned\`"
  fi

  validate_specs "$ANALYST_STATE_DIR" "$ANALYST_DIR/analyst/analyst.yaml"
  validate_specs "$DATA_OWNER_STATE_DIR" "$DATA_OWNER_DIR/data-owner/data-owner.yaml"

  register_destroy_target "$ANALYST_STATE_DIR" analyst-agent
  register_destroy_target "$DATA_OWNER_STATE_DIR" data-owner-agent

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    log "building analyst agent image"
    ca_run "$ANALYST_STATE_DIR" build --spec "$ANALYST_DIR/analyst/analyst.yaml"
    record_manifest_variants "$ANALYST_STATE_DIR" analyst-agent
    log "building data owner agent image"
    ca_run "$DATA_OWNER_STATE_DIR" build --spec "$DATA_OWNER_DIR/data-owner/data-owner.yaml"
    record_manifest_variants "$DATA_OWNER_STATE_DIR" data-owner-agent
  fi

  ensure_operator_peering "$ANALYST_STATE_DIR" analyst-ops "$allowed_cidr"
  ensure_operator_peering "$DATA_OWNER_STATE_DIR" data-owner-ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    ca_run "$ANALYST_STATE_DIR" deploy --spec "$ANALYST_DIR/analyst/analyst.yaml"
    ca_run "$DATA_OWNER_STATE_DIR" deploy --spec "$DATA_OWNER_DIR/data-owner/data-owner.yaml"
  fi

  wait_for_status_service_ready "$ANALYST_STATE_DIR" analyst-agent 900
  wait_for_status_service_ready "$DATA_OWNER_STATE_DIR" data-owner-agent 900

  local analyst_ip data_owner_ip analyst_private_ip data_owner_private_ip
  analyst_ip="$(state_value "$ANALYST_STATE_DIR" analyst-agent deploy.public_ip)"
  data_owner_ip="$(state_value "$DATA_OWNER_STATE_DIR" data-owner-agent deploy.public_ip)"
  analyst_private_ip="$(state_value "$ANALYST_STATE_DIR" analyst-agent deploy.private_ip)"
  data_owner_private_ip="$(state_value "$DATA_OWNER_STATE_DIR" data-owner-agent deploy.private_ip)"
  if [[ -z "$analyst_ip" || -z "$data_owner_ip" || -z "$analyst_private_ip" || -z "$data_owner_private_ip" ]]; then
    echo "a2a-data-collab requires both deployed services to report public_ip and private_ip" >&2
    return 1
  fi
  local analyst_key data_owner_key
  analyst_key="$(state_value "$ANALYST_STATE_DIR" analyst-agent build.debug_ssh.private_key)"
  data_owner_key="$(state_value "$DATA_OWNER_STATE_DIR" data-owner-agent build.debug_ssh.private_key)"
  chmod 0600 "$analyst_key" "$data_owner_key"

  ca_run "$ANALYST_STATE_DIR" peering add --role peer --cidr "$data_owner_ip/32" --label data-owner-public
  if [[ "$data_owner_private_ip" != "$data_owner_ip" ]]; then
    ca_run "$ANALYST_STATE_DIR" peering add --role peer --cidr "$data_owner_private_ip/32" --label data-owner-private
  fi
  ca_run "$DATA_OWNER_STATE_DIR" peering add --role peer --cidr "$analyst_ip/32" --label analyst-public
  if [[ "$analyst_private_ip" != "$analyst_ip" ]]; then
    ca_run "$DATA_OWNER_STATE_DIR" peering add --role peer --cidr "$analyst_private_ip/32" --label analyst-private
  fi
  ca_run "$ANALYST_STATE_DIR" peering apply
  ca_run "$DATA_OWNER_STATE_DIR" peering apply

  curl -fsS "http://$data_owner_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/data-owner-agent-card.json"
  if data_collab_signing_enabled; then
    assert_agent_card_signed "$WORK_DIR/data-owner-agent-card.json"
  else
    assert_agent_card_unsigned "$WORK_DIR/data-owner-agent-card.json"
  fi
  record_file_as_block "Data Owner AgentCard:" "$WORK_DIR/data-owner-agent-card.json" json

  curl -fsS "http://$analyst_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/analyst-agent-card.json"
  if data_collab_signing_enabled; then
    assert_agent_card_signed "$WORK_DIR/analyst-agent-card.json"
  else
    assert_agent_card_unsigned "$WORK_DIR/analyst-agent-card.json"
  fi
  record_file_as_block "Analyst AgentCard:" "$WORK_DIR/analyst-agent-card.json" json

  if data_collab_signing_enabled; then
    ca_run "$ANALYST_STATE_DIR" a2a add \
      --alias wrong-data-owner \
      --signer-issuer "$A2A_SIGNER_ISSUER" \
      --signer-subject "${A2A_SIGNER_SUBJECT}-wrong" \
      "http://$data_owner_ip:8089/.well-known/agent-card.json"
    wait_for_a2a_peer_state "$analyst_ip" wrong-data-owner error "signature|verify|certificate|identity" 300
    ca_run "$ANALYST_STATE_DIR" a2a remove wrong-data-owner

    ca_run "$ANALYST_STATE_DIR" a2a add \
      --alias data-owner \
      --signer-issuer "$A2A_SIGNER_ISSUER" \
      --signer-subject "$A2A_SIGNER_SUBJECT" \
      "http://$data_owner_ip:8089/.well-known/agent-card.json"

    ca_run "$DATA_OWNER_STATE_DIR" a2a add \
      --alias analyst \
      --signer-issuer "$A2A_SIGNER_ISSUER" \
      --signer-subject "$A2A_SIGNER_SUBJECT" \
      "http://$analyst_ip:8089/.well-known/agent-card.json"
  else
    ca_run "$ANALYST_STATE_DIR" a2a add \
      --alias data-owner \
      "http://$data_owner_ip:8089/.well-known/agent-card.json"

    ca_run "$DATA_OWNER_STATE_DIR" a2a add \
      --alias analyst \
      "http://$analyst_ip:8089/.well-known/agent-card.json"
  fi

  wait_for_status_service_ready "$ANALYST_STATE_DIR" analyst-agent 900
  wait_for_a2a_peer_state "$analyst_ip" data-owner ok "" 300
  wait_for_a2a_peer_state "$data_owner_ip" analyst ok "" 300
  assert_a2a_guest_tng_28_config analyst "$analyst_ip" "$analyst_key"
  assert_a2a_guest_tng_28_config data-owner "$data_owner_ip" "$data_owner_key"
  assert_a2a_connect_boundary analyst "$analyst_ip" "$analyst_key" data-owner
  assert_a2a_connect_boundary data-owner "$data_owner_ip" "$data_owner_key" analyst
  ca_run "$ANALYST_STATE_DIR" status --live | tee "$WORK_DIR/status-live.txt"
  record_file_as_block "Analyst live status output:" "$WORK_DIR/status-live.txt" text

  local connect_port
  connect_port="$(start_connect_until_http_ready "$ANALYST_STATE_DIR" analyst-agent /health 4 180 --service analyst-agent)"
  record "Connect mapped Analyst Agent to \`127.0.0.1:$connect_port\`."

  if ! node "$ROOT_DIR/tools/e2e/probes/a2a-data-collab-probe.mjs" \
    --url "http://127.0.0.1:$connect_port/a2a" \
    --message "$TASK_MESSAGE" \
    --timeout-ms "$CHAT_TIMEOUT_MS" \
    >"$WORK_DIR/a2a-data-collab-result.json" \
    2>"$WORK_DIR/a2a-data-collab-probe.err"; then
    record_file_as_block "A2A data collaboration probe stderr:" "$WORK_DIR/a2a-data-collab-probe.err" text
    fetch_data_collab_guest_diagnostics analyst "$analyst_ip" "$analyst_key"
    fetch_data_collab_guest_diagnostics data-owner "$data_owner_ip" "$data_owner_key"
    return 1
  fi
  record_file_as_block "A2A data collaboration result:" "$WORK_DIR/a2a-data-collab-result.json" json

  run_report_probe "$ANALYST_STATE_DIR" "$WORK_DIR/analyst-attestation-report.json" analyst-agent data-owner
  run_report_probe "$DATA_OWNER_STATE_DIR" "$WORK_DIR/data-owner-attestation-report.json" data-owner-agent analyst
}
