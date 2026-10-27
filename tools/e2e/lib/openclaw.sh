#!/usr/bin/env bash

cleanup_openclaw_init_connect() {
  local status="$1"
  local pid_path="${INIT_OUTPUT_DIR:-}/connect.pid"
  if [[ -n "$pid_path" && -f "$pid_path" ]]; then
    local pid
    pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
    rm -f "$pid_path"
  fi
  record "- init local connect cleanup completed for status \`$status\`."
}

assert_openclaw_gateway_config() {
  local config="$OPENCLAW_DIR/openclaw.json"
  jq -e '
    .gateway.auth.mode == "token" and
    ((.gateway.auth.token // "") | length) >= 32 and
    .gateway.http.endpoints.responses.enabled == true and
    .gateway.controlUi.enabled == true and
    .gateway.controlUi.dangerouslyDisableDeviceAuth == true
  ' "$config" >/dev/null
  record "- OpenClaw gateway config uses token auth with device auth disabled for the init control UI."
}

assert_openclaw_init_output() {
  local spec="$OPENCLAW_DIR/openclaw.yaml"
  local install_script="$OPENCLAW_DIR/install-openclaw.sh"

  grep -Fq "packages: [ca-certificates, curl, findutils, git, jq, kernel, kmod, nodejs, npm, podman, tar, xz]" "$spec"
  grep -Fq "source: ./files/install-openclaw-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh" "$spec"
  grep -Fq "source: ./files/cai-a2a-plugin" "$spec"
  grep -Fq "scripts: [./install-openclaw.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: /root/.openclaw/openclaw.json" "$spec"
  grep -Fq "OPENCLAW_VERSION" "$install_script"
  grep -Fq "OPENCLAW_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "CA_DISABLE_PEP" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/openclaw/install-openclaw.sh" \
    OPENCLAW_VERSION OPENCLAW_NODE_VERSION NPM_REGISTRY CA_DISABLE_PEP

  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    ! grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    ! grep -Fq "patch-openclaw-cai-pep.js" "$spec"
    ! grep -Fq "cai-pep-default-policy.json" "$spec"
  else
    grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    grep -Fq "target: /root/.openclaw/skills/tdx-remote-attestation/SKILL.md" "$spec"
    grep -Fq "source: ./files/cai-pep-plugin" "$spec"
    grep -Fq "patch-openclaw-cai-pep.js" "$spec"
  fi
  record "- OpenClaw init output mirrors the example files and rendered PEP mode."
}

run_openclaw_gateway_token_probe() {
  local connect_port="$1"
  local token="$2"
  require_cmd openclaw
  local valid_state invalid_state client_config
  valid_state="$(mktemp -d "$WORK_DIR/openclaw-probe-valid.XXXXXX")"
  invalid_state="$(mktemp -d "$WORK_DIR/openclaw-probe-invalid.XXXXXX")"
  client_config="$WORK_DIR/openclaw-probe-client.json"
  printf '%s\n' '{"gateway":{"mode":"local"}}' > "$client_config"
  local openclaw_env=(
    env
    -u OPENCLAW_GATEWAY_TOKEN -u OPENCLAW_GATEWAY_PASSWORD
    "OPENCLAW_CONFIG_PATH=$client_config"
  )
  local url="ws://127.0.0.1:$connect_port"

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$valid_state") OPENCLAW_CONFIG_PATH=$(printf '%q' "$client_config") openclaw gateway probe --url $url --token '<redacted>' --json --timeout 10000"
  "${openclaw_env[@]}" "OPENCLAW_STATE_DIR=$valid_state" openclaw gateway probe \
    --url "$url" \
    --token "$token" \
    --json \
    --timeout 10000 \
    >"$WORK_DIR/gateway-probe.json" 2>"$WORK_DIR/gateway-probe.err"
  record_file_as_block "OpenClaw gateway token probe:" "$WORK_DIR/gateway-probe.json" json
  record_file_as_block "OpenClaw gateway token probe stderr:" "$WORK_DIR/gateway-probe.err" text
  jq -e '
    .ok == true and
    any(.targets[]?; .id == "explicit" and .connect.ok == true and (.connect.rpcOk == true or .connect.scopeLimited == true))
  ' \
    "$WORK_DIR/gateway-probe.json" >/dev/null

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$invalid_state") OPENCLAW_CONFIG_PATH=$(printf '%q' "$client_config") openclaw gateway probe --url $url --token '<wrong-token>' --json --timeout 10000"
  local wrong_status=0
  "${openclaw_env[@]}" "OPENCLAW_STATE_DIR=$invalid_state" openclaw gateway probe \
      --url "$url" \
      --token "ca-e2e-wrong-token" \
      --json \
      --timeout 10000 \
      >"$WORK_DIR/gateway-probe-wrong-token.json" 2>"$WORK_DIR/gateway-probe-wrong-token.err" || wrong_status=$?
  record_file_as_block "OpenClaw wrong-token probe:" "$WORK_DIR/gateway-probe-wrong-token.json" json
  record_file_as_block "OpenClaw wrong-token probe stderr:" "$WORK_DIR/gateway-probe-wrong-token.err" text
  # Recent probes report transport reachability independently of RPC auth and
  # can exit zero after an authentication close. Require the actual rejection;
  # a connection error alone is not evidence that token validation worked.
  if ! jq -e 'any(.targets[]?;
      .id == "explicit" and .connect.rpcOk != true and .connect.scopeLimited != true and
      (([.connect.error, .connect.close.reason] | map(select(. != null) | tostring) | join(" "))
        | test("unauthorized|token.*mismatch|invalid.*token"; "i")))' \
      "$WORK_DIR/gateway-probe-wrong-token.json" >/dev/null; then
    echo "OpenClaw wrong-token probe did not prove authentication rejection (exit $wrong_status)" >&2
    return 1
  fi
  record "- OpenClaw gateway accepts the configured token and rejects an invalid token."
}

run_openclaw_pep_e2e() {
  local connect_port="$1" token="$2" host key remote_probe proof_id proof_dir proof_hash
  host="$(state_value "$STATE_DIR" openclaw deploy.public_ip)"
  key="$(state_value "$STATE_DIR" openclaw build.debug_ssh.private_key)"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  remote_probe="/tmp/ca-pep-e2e-$E2E_RUN_ID.mjs"
  ssh_guest "$key" "$host" "umask 077; cat > '$remote_probe'" \
    <"$ROOT_DIR/tools/e2e/probes/openclaw-pep-probe.mjs"
  local remote_output="$remote_probe.report.json"
  local -a probe_args=(node "$remote_probe" --output "$remote_output")
  if [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    probe_args+=(--tdx)
  fi
  local probe_status=0
  ssh_guest "$key" "$host" "${probe_args[@]}" \
    >"$WORK_DIR/pep-enforcement-probe.json" 2>"$WORK_DIR/pep-enforcement-probe.err" || probe_status=$?
  if [[ "$probe_status" != "0" ]]; then
    ssh_guest "$key" "$host" "cat '$remote_output' 2>/dev/null || true" >"$WORK_DIR/pep-enforcement-probe.remote.json"
    if jq -e '.ok == false' "$WORK_DIR/pep-enforcement-probe.remote.json" >/dev/null 2>&1; then
      cp "$WORK_DIR/pep-enforcement-probe.remote.json" "$WORK_DIR/pep-enforcement-probe.json"
    fi
    ssh_guest "$key" "$host" "cat '$remote_output.gateway.log' 2>/dev/null || true" >"$WORK_DIR/pep-enforcement-gateway.log"
    ssh_guest "$key" "$host" "cat '$remote_output.pep.log' 2>/dev/null || true" >"$WORK_DIR/pep-enforcement-pep.log"
    record_file_as_block "Private PEP probe gateway diagnostics:" "$WORK_DIR/pep-enforcement-gateway.log" text
    record_file_as_block "Private PEP probe daemon diagnostics:" "$WORK_DIR/pep-enforcement-pep.log" text
  fi
  ssh_guest "$key" "$host" "rm -f '$remote_probe' '$remote_output' '$remote_output.gateway.log' '$remote_output.pep.log'" || true
  record_file_as_block "OpenClaw exec PEP enforcement probe:" "$WORK_DIR/pep-enforcement-probe.json" json
  record_file_as_block "OpenClaw exec PEP enforcement probe stderr:" "$WORK_DIR/pep-enforcement-probe.err" text
  [[ "$probe_status" == "0" ]] || return "$probe_status"
  jq -e '.ok == true and .scope == "exec only"' "$WORK_DIR/pep-enforcement-probe.json" >/dev/null

  # The model never sees the random challenge or its digest before executing.
  # An echoed marker cannot satisfy digest, exactly-once mutation, and PEP audit.
  proof_id="pep-$(openssl rand -hex 12)"
  proof_dir="/workspace/ca-e2e-$proof_id"
  proof_hash="$(ssh_guest "$key" "$host" "umask 077; mkdir '$proof_dir'; openssl rand 64 > '$proof_dir/challenge'; sha256sum '$proof_dir/challenge' | cut -d ' ' -f 1")"
  [[ "$proof_hash" =~ ^[0-9a-f]{64}$ ]] || return 1
  local command="sha256sum $proof_dir/challenge && printf x >> $proof_dir/executions" message
  printf -v message 'Use the exec tool exactly once with workdir /workspace to run this exact command:\n%s\nReturn the SHA-256 printed by the tool. You must execute the tool; do not invent a hash.' "$command"
  run_openclaw_chat_probe \
    "ws://127.0.0.1:$connect_port" "$token" \
    "$message" \
    "$proof_hash" "$WORK_DIR/pep-bailian-exec-probe.json" \
    --session "agent:main:ca-e2e-$proof_id" \
    --instructions "Execute the requested exec command once, then report its actual stdout. Do not read the challenge with other tools." \
    --timeout-ms "$CHAT_TIMEOUT_MS"

  ssh_guest "$key" "$host" node --input-type=module - "$proof_id" "$proof_dir" \
    >"$WORK_DIR/pep-bailian-audit-proof.json" <<'JS'
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
const [proofId, proofDir] = process.argv.slice(2);
assert.equal(fs.readFileSync(path.join(proofDir, "executions"), "utf8"), "x", "exec did not run exactly once");
const raw = execFileSync("journalctl", ["-u", "cai-pep.service", "--no-pager", "-o", "cat"], { encoding: "utf8" });
const events = raw.split("\n").flatMap(line => {
  try {
    const event = JSON.parse(line);
    return String(event.session_key ?? "").includes(proofId) && event.event === "intent_allow" ? [event] : [];
  } catch { return []; }
});
assert.equal(events.length, 1, "expected exactly one correlated live PEP allow event");
const event = events[0];
assert.equal(event.tool_name, "exec");
assert.equal(event.backend, "docker");
assert.equal(event.exit_code, 0);
assert.ok(event.command.includes(proofId) && event.command.startsWith("sha256sum "));
assert.ok(event.audit_id && event.run_id && event.request_id);
console.log(JSON.stringify({ ok: true, exactlyOnce: true, backend: event.backend,
  auditId: event.audit_id, runId: event.run_id, requestId: event.request_id,
  sessionKey: event.session_key }, null, 2));
JS
  record_file_as_block "Bailian exec side effect and correlated real PEP audit:" "$WORK_DIR/pep-bailian-audit-proof.json" json
  ssh_guest "$key" "$host" "rm -rf '$proof_dir'"
  record "- Real Bailian exec returned the unseen challenge digest, executed once in the PEP container, and correlated run/session/request/audit IDs."
}
