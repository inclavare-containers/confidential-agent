#!/usr/bin/env bash

# shellcheck source=tools/e2e/lib/openclaw.sh
source "$ROOT_DIR/tools/e2e/lib/openclaw.sh"
# shellcheck source=tools/e2e/lib/codex.sh
source "$ROOT_DIR/tools/e2e/lib/codex.sh"
# shellcheck source=tools/e2e/lib/trustee.sh
source "$ROOT_DIR/tools/e2e/lib/trustee.sh"

declare -Ag SERVICE_INSTANCE_ID=()
declare -Ag SERVICE_VPC_ID=()
declare -Ag SERVICE_VSWITCH_ID=()
declare -Ag SERVICE_SECURITY_GROUP_ID=()
declare -Ag SERVICE_IMAGE_ID=()
declare -Ag SERVICE_BUCKET=()
declare -Ag SERVICE_TERRAFORM_DIR=()

case_cleanup() {
  local status="$1"
  local output_dir
  for output_dir in "${OPENCLAW_INIT_OUTPUT_DIR:-}" "${CODEX_INIT_OUTPUT_DIR:-}"; do
    [[ -n "$output_dir" ]] || continue
    INIT_OUTPUT_DIR="$output_dir" cleanup_openclaw_init_connect "$status"
  done
  if [[ "$TRUSTEE_INFRA_CREATED" == "1" && -n "$TRUSTEE_PUBLIC_IP" ]]; then
    trustee_capture_diagnostics "cleanup-$status" || true
    trustee_start >/dev/null 2>&1 || true
  fi
}

case_post_destroy_cleanup() {
  local status="$1"
  local should_destroy=0
  if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
    should_destroy=1
  elif [[ "$status" != "0" && "$DESTROY_ON_FAILURE" == "1" ]]; then
    should_destroy=1
  fi
  [[ "$should_destroy" == "1" ]] || return 0
  # Failed application deletion still needs Trustee to authorize a retry.
  local service terraform_dir phase
  for service in openclaw codex; do
    [[ -f "$STATE_DIR/services/$service/state.json" ]] || continue
    terraform_dir="$(state_value "$STATE_DIR" "$service" deploy.terraform_dir)" || return 1
    phase="$(state_value "$STATE_DIR" "$service" phase)" || return 1
    if [[ "$phase" != deleted ]] && ! python3.11 - "$STATE_DIR/services/$service/state.json" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
observed = ('terraform_dir', 'image_source', 'bucket', 'instance_id', 'security_group_id',
            'private_ip', 'public_ip', 'published_image_id', 'instance_ids',
            'instance_private_ips', 'instance_public_ips', 'slb_vip')
local_only = (state.get('phase') == 'built' and not (state.get('build') or {}).get('published')
              and not any((state.get('deploy') or {}).get(name) for name in observed))
raise SystemExit(0 if local_only else 1)
PY
    then
      log "keeping Trustee available while $service cleanup is incomplete"
      return 1
    fi
  done
  destroy_trustee_infra || return
  assert_trustee_cloud_zero || return
  # The disposable Trustee is now proven absent, including resources uploaded
  # by a failed first sync. Only then release builds that never launched a VM.
  for service in openclaw codex; do
    [[ -f "$STATE_DIR/services/$service/state.json" ]] || continue
    phase="$(state_value "$STATE_DIR" "$service" phase)" || return 1
    if [[ "$phase" == built ]]; then
      ca_run "$STATE_DIR" image rm "$service" --force || return
    fi
  done
}

assert_challenge_mode() {
  local spec="$1"
  python3.11 - "$spec" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
challenge = re.findall(r"(?m)^  mode: challenge\s*$", text)
trustee = re.findall(r"(?m)^  mode: trustee\s*$", text)
if len(challenge) != 1 or trustee:
    raise SystemExit(f"{path} must contain exactly one challenge mode and no trustee mode")
PY
}

switch_generated_spec_to_trustee() {
  local spec="$1"
  assert_challenge_mode "$spec"
  python3.11 - "$spec" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
updated, count = re.subn(r"(?m)^  mode: challenge\s*$", "  mode: trustee", text)
if count != 1:
    raise SystemExit(f"expected exactly one challenge mode in {path}, got {count}")
path.write_text(updated, encoding="utf-8")
PY
  grep -Fxq "  mode: trustee" "$spec"
  record "- switched generated work-directory spec \`$spec\` to Trustee mode."
}

prepare_openclaw_fixture() {
  local -a init_args=()
  mapfile -d '' init_args < <(init_common_args "$OPENCLAW_INIT_OUTPUT_DIR" "$OPENCLAW_DISK_GB")
  init_args+=(
    --dashscope-api-key "$DASHSCOPE_KEY_VALUE"
    --gateway-token "$OPENCLAW_TOKEN"
    --model "$DASHSCOPE_MODEL"
    --dashscope-base-url "$DASHSCOPE_BASE_URL"
    --openclaw-version "${E2E_OPENCLAW_VERSION:-2026.10.3}"
    --node-version "${E2E_OPENCLAW_NODE_VERSION:-24.21.0}"
    --npm-registry "${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  )
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/openclaw-init.out" "$WORK_DIR/openclaw-init.err" openclaw "${init_args[@]}"; then
    record_file_as_block "OpenClaw init stdout:" "$WORK_DIR/openclaw-init.out" text
    record_file_as_block "OpenClaw init stderr:" "$WORK_DIR/openclaw-init.err" text
    return 1
  fi
  record_file_as_block "OpenClaw init stdout:" "$WORK_DIR/openclaw-init.out" text
  record_file_as_block "OpenClaw init stderr:" "$WORK_DIR/openclaw-init.err" text
  assert_openclaw_gateway_config
  assert_openclaw_init_output
}

prepare_codex_fixture() {
  local -a init_args=()
  mapfile -d '' init_args < <(init_common_args "$CODEX_INIT_OUTPUT_DIR" "$CODEX_DISK_GB")
  init_args+=(
    --dashscope-api-key "$DASHSCOPE_KEY_VALUE"
    --dashscope-base-url "$DASHSCOPE_BASE_URL"
    --model "$DASHSCOPE_MODEL"
    --codex-app-server-token "$CODEX_REMOTE_TOKEN_VALUE"
    --node-version "${E2E_CLI_AGENT_NODE_VERSION:-24.21.0}"
    --npm-registry "${E2E_CLI_AGENT_NPM_REGISTRY:-${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}}"
    --codex-version "${E2E_CODEX_VERSION:-$(ca_software_version codex)}"
  )
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/codex-init.out" "$WORK_DIR/codex-init.err" codex "${init_args[@]}"; then
    record_file_as_block "Codex init stdout:" "$WORK_DIR/codex-init.out" text
    record_file_as_block "Codex init stderr:" "$WORK_DIR/codex-init.err" text
    return 1
  fi
  record_file_as_block "Codex init stdout:" "$WORK_DIR/codex-init.out" text
  record_file_as_block "Codex init stderr:" "$WORK_DIR/codex-init.err" text
  assert_codex_secret_rendering "$DASHSCOPE_KEY_VALUE" "$CODEX_REMOTE_TOKEN_VALUE"
}

capture_service_cloud() {
  local service="$1"
  local instance_id image_name image_out describe_out terraform_out
  instance_id="$(state_value "$STATE_DIR" "$service" deploy.instance_id)"
  image_name="$(state_value "$STATE_DIR" "$service" deploy.image_import_name)"
  SERVICE_TERRAFORM_DIR["$service"]="$(state_value "$STATE_DIR" "$service" deploy.terraform_dir)"
  [[ -n "$instance_id" && -n "$image_name" && -n "${SERVICE_TERRAFORM_DIR[$service]}" ]] || {
    echo "missing cloud state for $service" >&2
    return 1
  }
  terraform_out="$WORK_DIR/$service-terraform-state.json"
  terraform -chdir="${SERVICE_TERRAFORM_DIR[$service]}" show -json >"$terraform_out"
  SERVICE_BUCKET["$service"]="$(python3.11 - "$terraform_out" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)

def resources(module):
    yield from module.get("resources") or []
    for child in module.get("child_modules") or []:
        yield from resources(child)

root = ((state.get("values") or {}).get("root_module") or {})
buckets = sorted({
    str((item.get("values") or {}).get("bucket") or "").strip()
    for item in resources(root)
    if item.get("type") == "alicloud_oss_bucket" and item.get("name") == "shelter_images"
})
buckets = [bucket for bucket in buckets if bucket]
if len(buckets) != 1:
    raise SystemExit(f"expected exactly one Shelter image bucket, got {buckets}")
print(buckets[0])
PY
)"
  describe_out="$WORK_DIR/$service-instance.json"
  trustee_aliyun_cli ecs DescribeInstances \
    --RegionId "$REGION" \
    --InstanceIds "[\"$instance_id\"]" >"$describe_out"
  SERVICE_INSTANCE_ID["$service"]="$instance_id"
  SERVICE_VPC_ID["$service"]="$(jq -er '.Instances.Instance[0].VpcAttributes.VpcId' "$describe_out")"
  SERVICE_VSWITCH_ID["$service"]="$(jq -er '.Instances.Instance[0].VpcAttributes.VSwitchId' "$describe_out")"
  SERVICE_SECURITY_GROUP_ID["$service"]="$(jq -er '.Instances.Instance[0].SecurityGroupIds.SecurityGroupId[0]' "$describe_out")"
  image_out="$WORK_DIR/$service-image.json"
  trustee_aliyun_cli ecs DescribeImages \
    --Status ALL \
    --RegionId "$REGION" \
    --ImageOwnerAlias self \
    --ImageName "$image_name" >"$image_out"
  SERVICE_IMAGE_ID["$service"]="$(jq -er --arg name "$image_name" '[.Images.Image[] | select(.ImageName == $name)][0].ImageId' "$image_out")"
  record_file_as_block "$service instance:" "$describe_out" json
  record_file_as_block "$service imported image:" "$image_out" json
}

assert_trustee_guest_tng_28() {
  local service="$1"
  local host key output check
  host="$(state_value "$STATE_DIR" "$service" deploy.public_ip)"
  key="$(state_value "$STATE_DIR" "$service" build.debug_ssh.private_key)"
  output="$WORK_DIR/$service-tng-runtime.txt"
  [[ -n "$host" && -f "$key" ]] || {
    echo "missing debug connection details for $service TNG check" >&2
    return 1
  }
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  check="set -euo pipefail; rpm_nevra=\$(rpm -q trusted-network-gateway); test \"\$rpm_nevra\" = trusted-network-gateway-2.9.2-1.al8.x86_64; tng_version=\$(/usr/bin/tng --version | sed -n '1p'); test \"\$tng_version\" = 'tng 2.9.2'; jq -e '([(.add_ingress // [])[], (.add_egress // [])[]]) as \$routes | (\$routes | length > 0) and (\$routes | all(.rats_tls.multiplex == true)) and ([\$routes[] | select(.verify? != null)] | length > 0) and ([\$routes[] | select(.verify? != null)] | all(.verify.as_type == \"builtin\" and .verify.attestation_policy.type == \"path\" and ((.verify | has(\"policy\")) | not) and ((.verify | has(\"policy_ids\")) | not)))' /etc/tng/config.json >/dev/null; curl -fsS http://127.0.0.1:50000/status/ >/dev/null; printf 'tng_rpm=%s\\ntng_version=%s\\ntng_config_schema=2.9.2\\n' \"\$rpm_nevra\" \"\$tng_version\""

  record_cmd "ssh $service '<assert Trustee guest TNG 2.8 config>'"
  ssh_guest "$key" "$host" "$check" >"$output"
  record_file_as_block "$service Trustee guest TNG runtime:" "$output" text
}

assert_oss_bucket_absent() {
  local bucket="$1"
  local output
  if output="$(trustee_aliyun_cli oss stat "oss://$bucket" --region "$REGION" 2>&1)"; then
    return 1
  fi
  if grep -Fq "ErrorCode=NoSuchBucket" <<<"$output"; then
    return 0
  fi
  printf 'failed to verify OSS bucket %s absence: %s\n' "$bucket" "$output" >&2
  return 2
}

assert_independent_service_infra() {
  local left="$1"
  local right="$2"
  [[ "${SERVICE_INSTANCE_ID[$left]}" != "${SERVICE_INSTANCE_ID[$right]}" ]]
  [[ "${SERVICE_VPC_ID[$left]}" != "${SERVICE_VPC_ID[$right]}" ]]
  [[ "${SERVICE_SECURITY_GROUP_ID[$left]}" != "${SERVICE_SECURITY_GROUP_ID[$right]}" ]]
  [[ "${SERVICE_VPC_ID[$left]}" != "$TRUSTEE_VPC_ID" ]]
  [[ "${SERVICE_VPC_ID[$right]}" != "$TRUSTEE_VPC_ID" ]]
  [[ "${SERVICE_SECURITY_GROUP_ID[$left]}" != "$TRUSTEE_SECURITY_GROUP_ID" ]]
  [[ "${SERVICE_SECURITY_GROUP_ID[$right]}" != "$TRUSTEE_SECURITY_GROUP_ID" ]]
  record "- OpenClaw, Codex, and Trustee use three independent VPC/security-group pairs."
}

assert_service_cloud_gone() {
  local service="$1"
  local deadline=$((SECONDS + ${2:-600}))
  while (( SECONDS < deadline )); do
    local instances groups vpcs switches images state_count bucket_present=0
    instances="$(trustee_aliyun_cli ecs DescribeInstances --RegionId "$REGION" --InstanceIds "[\"${SERVICE_INSTANCE_ID[$service]}\"]" | jq -r '.TotalCount')"
    groups="$(trustee_aliyun_cli ecs DescribeSecurityGroups --RegionId "$REGION" --SecurityGroupIds "[\"${SERVICE_SECURITY_GROUP_ID[$service]}\"]" | jq -r '.TotalCount')"
    vpcs="$(trustee_aliyun_cli vpc DescribeVpcs --RegionId "$REGION" --VpcId "${SERVICE_VPC_ID[$service]}" | jq -r '.TotalCount')"
    switches="$(trustee_aliyun_cli vpc DescribeVSwitches --RegionId "$REGION" --VSwitchId "${SERVICE_VSWITCH_ID[$service]}" | jq -r '.TotalCount')"
    images="$(trustee_aliyun_cli ecs DescribeImages --Status ALL --RegionId "$REGION" --ImageId "${SERVICE_IMAGE_ID[$service]}" | jq -r '.TotalCount')"
    state_count="$(terraform -chdir="${SERVICE_TERRAFORM_DIR[$service]}" state list | wc -l)"
    if assert_oss_bucket_absent "${SERVICE_BUCKET[$service]}"; then
      bucket_present=0
    else
      local bucket_status=$?
      if [[ "$bucket_status" == "1" ]]; then
        bucket_present=1
      else
        return "$bucket_status"
      fi
    fi
    if [[ "$instances" == "0" && "$groups" == "0" && "$vpcs" == "0" && "$switches" == "0" && "$images" == "0" && "$state_count" == "0" && "$bucket_present" == "0" ]]; then
      record "- $service instance, image, bucket, SG, vSwitch, VPC, and Terraform state are zero."
      return 0
    fi
    sleep 15
  done
  echo "$service cloud resources did not reach exact zero" >&2
  return 1
}

assert_local_lifecycle_status() {
  local active_csv="$1"
  local deleted_csv="$2"
  local label="$3"
  local out="$WORK_DIR/status-$label.json"
  ca_capture "$STATE_DIR" "$out" "$WORK_DIR/status-$label.err" status --json --live || return
  record_file_as_block "Local/live status ($label):" "$out" json
  python3.11 - "$out" "$active_csv" "$deleted_csv" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    items = json.load(f)
active = {item for item in sys.argv[2].split(",") if item}
deleted = {item for item in sys.argv[3].split(",") if item}
seen = set()
for item in items:
    local = item.get("local") or item
    service_id = local.get("service_id")
    if service_id in seen:
        raise SystemExit(f"duplicate service in status: {service_id}")
    seen.add(service_id)
    phase = local.get("phase")
    if service_id in active:
        if phase != "active":
            raise SystemExit(f"{service_id} phase is {phase}, expected active")
        daemon = item.get("daemon") or {}
        if daemon.get("phase") != "running" or daemon.get("app_ready") is not True or daemon.get("mesh_ready") is not True:
            raise SystemExit(f"{service_id} daemon is not ready: {daemon}")
        generation = local.get("mesh_generation")
        if generation is None or daemon.get("mesh_generation") != generation:
            raise SystemExit(f"{service_id} daemon has not applied local mesh generation {generation}")
        if item.get("live_error"):
            raise SystemExit(f"{service_id} live status failed: {item['live_error']}")
    elif service_id in deleted:
        if phase != "deleted":
            raise SystemExit(f"{service_id} phase is {phase}, expected deleted")
    else:
        raise SystemExit(f"unexpected service in status: {service_id}")
if seen != active | deleted:
    raise SystemExit(f"status services mismatch: {seen} != {active | deleted}")
PY
}

wait_for_local_lifecycle_status() {
  local active_csv="$1" deleted_csv="$2" label="$3" timeout="$4"
  local deadline=$((SECONDS + timeout)) attempt=0
  while (( SECONDS < deadline )); do
    attempt=$((attempt + 1))
    if assert_local_lifecycle_status "$active_csv" "$deleted_csv" "$label-attempt-$attempt"; then
      return 0
    fi
    sleep 5
  done
  echo "timed out waiting for joint local/live lifecycle status: $label" >&2
  return 1
}

run_openclaw_conversation() {
  local label="$1"
  local connect_port
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" "trustee-openclaw-$label" /openclaw 4 180 --service openclaw)"
  if [[ "$label" == "initial" ]]; then
    run_openclaw_gateway_token_probe "$connect_port" "$OPENCLAW_TOKEN"
  fi
  local attempt probe_status
  for attempt in $(seq 1 "$CHAT_ATTEMPTS"); do
    if run_openclaw_chat_probe \
      "ws://127.0.0.1:$connect_port" \
      "$OPENCLAW_TOKEN" \
      "$OPENCLAW_CHAT_MESSAGE" \
      "$OPENCLAW_CHAT_EXPECT" \
      "$WORK_DIR/openclaw-chat-$label-$attempt.json" \
      --session "confidential-agent-trustee-$label-$E2E_RUN_ID-$attempt" \
      --timeout-ms "$CHAT_TIMEOUT_MS"; then
      return 0
    else
      probe_status=$?
    fi
    record "- OpenClaw chat ($label) attempt $attempt/$CHAT_ATTEMPTS failed with status $probe_status."
    if ((attempt < CHAT_ATTEMPTS)); then
      sleep 30
    fi
  done
  echo "OpenClaw chat ($label) failed after $CHAT_ATTEMPTS attempts" >&2
  return 1
}

run_codex_conversation() {
  local host key connect_port
  host="$(state_value "$STATE_DIR" codex deploy.public_ip)"
  key="$(state_value "$STATE_DIR" codex build.debug_ssh.private_key)"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  assert_codex_guest_runtime "$host" "$key"
  if ! run_codex_exec_prompt "$host" "$key" chat "$CODEX_CHAT_MESSAGE" "$CODEX_CHAT_EXPECT"; then
    collect_codex_guest_diagnostics "$host" "$key" "chat-failure"
    return 1
  fi
  if [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    local tdx_prompt='Use $tdx-remote-attestation to run the required remote attestation command and summarize the result. Include CA_TDX_SKILL_OK in the final answer.'
    run_codex_exec_prompt "$host" "$key" tdx-skill "$tdx_prompt" "TDX"
    grep -Fq "CA_TDX_SKILL_OK" "$WORK_DIR/codex-tdx-skill.out"
    grep -Eq "cai-pep|collect-and-verify|attest" "$WORK_DIR/codex-tdx-skill.out"
  fi
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" trustee-codex /readyz 4 180 --service codex)"
  run_codex_remote_probe "$connect_port" "$CODEX_REMOTE_TOKEN_VALUE"
}

reboot_openclaw_without_cli_sync() {
  local instance_id="${SERVICE_INSTANCE_ID[openclaw]}"
  local force_stop="${E2E_REBOOT_FORCE_STOP:-true}"
  [[ "$force_stop" == true || "$force_stop" == false ]] || {
    echo "E2E_REBOOT_FORCE_STOP must be true or false" >&2
    return 1
  }
  local public_ip
  public_ip="$(state_value "$STATE_DIR" openclaw deploy.public_ip)"
  local key sentinel before_boot after_boot guest_digest expected_digest
  key="$(state_value "$STATE_DIR" openclaw build.debug_ssh.private_key)"
  sentinel="$(openssl rand -hex 24)"
  wait_for_ssh "$public_ip" "$key" 300
  before_boot="$(ssh_guest "$key" "$public_ip" 'cat /proc/sys/kernel/random/boot_id')"
  expected_digest="$(sha256sum /usr/libexec/shelter/cryptpilot-fde-guest | awk '{print $1}')"
  guest_digest="$(ssh_guest "$key" "$public_ip" "sha256sum /usr/bin/cryptpilot-fde" | awk '{print $1}')"
  [[ "$guest_digest" == "$expected_digest" ]] || { echo "guest Cryptpilot differs from fde-guest" >&2; return 1; }
  ssh_guest "$key" "$public_ip" "install -d -m 0700 /var/lib/ca-e2e; printf '%s' '$sentinel' > /var/lib/ca-e2e/reboot-sentinel; sync"
  record "- OpenClaw reboot ForceStop=$force_stop."
  # Alibaba Cloud TDX instances do not support in-guest reboot. Both modes
  # must use the cloud API; boot ID and persistent data are checked below.
  trustee_aliyun_cli ecs RebootInstance \
    --RegionId "$REGION" \
    --InstanceId "$instance_id" \
    --ForceStop "$force_stop" >"$WORK_DIR/openclaw-reboot.json"
  record_file_as_block "OpenClaw reboot request:" "$WORK_DIR/openclaw-reboot.json" json
  local deadline=$((SECONDS + 180)) saw_down=0
  while (( SECONDS < deadline )); do
    if ! curl -fsS --max-time 3 "http://$public_ip:8088/status" >/dev/null 2>&1; then
      saw_down=1
      break
    fi
    sleep 1
  done
  [[ "$saw_down" == "1" ]] || {
    echo "OpenClaw status never went down during reboot" >&2
    return 1
  }
  wait_for_status_service_ready "$STATE_DIR" openclaw 1200
  local current_ip
  current_ip="$(trustee_aliyun_cli ecs DescribeInstances --RegionId "$REGION" --InstanceIds "[\"$instance_id\"]" | jq -er '.Instances.Instance[0].PublicIpAddress.IpAddress[0]')"
  [[ "$current_ip" == "$public_ip" ]] || {
    echo "OpenClaw public IP changed across reboot: $public_ip -> $current_ip" >&2
    return 1
  }
  wait_for_ssh "$public_ip" "$key" 300
  after_boot="$(ssh_guest "$key" "$public_ip" 'cat /proc/sys/kernel/random/boot_id')"
  [[ "$before_boot" != "$after_boot" ]] || { echo "boot ID did not change" >&2; return 1; }
  [[ "$(ssh_guest "$key" "$public_ip" 'cat /var/lib/ca-e2e/reboot-sentinel')" == "$sentinel" ]] || { echo "encrypted persistent delta lost data across reboot" >&2; return 1; }
  record "- Guest Cryptpilot matches fde-guest ($guest_digest); boot ID changed and encrypted delta sentinel survived reboot."
  record "- OpenClaw rebooted and recovered from Trustee without any CLI sync/inject."
}

assert_destroy_fails_closed_while_trustee_down() {
  local service="$1"
  local before_hash after_hash
  before_hash="$(sha256sum "$STATE_DIR/trustee/state.json" | awk '{print $1}')"
  trustee_stop
  if ca_capture "$STATE_DIR" "$WORK_DIR/$service-destroy-while-trustee-down.out" "$WORK_DIR/$service-destroy-while-trustee-down.err" destroy "$service"; then
    trustee_start || true
    echo "destroy unexpectedly succeeded while Trustee was stopped" >&2
    return 1
  fi
  trustee_start
  record_file_as_block "$service destroy while Trustee is down stdout:" "$WORK_DIR/$service-destroy-while-trustee-down.out" text
  record_file_as_block "$service destroy while Trustee is down stderr:" "$WORK_DIR/$service-destroy-while-trustee-down.err" text
  after_hash="$(sha256sum "$STATE_DIR/trustee/state.json" | awk '{print $1}')"
  [[ "$after_hash" == "$before_hash" ]]
  [[ "$(state_value "$STATE_DIR" "$service" phase)" == "active" ]]
  [[ "$(trustee_aliyun_cli ecs DescribeInstances --RegionId "$REGION" --InstanceIds "[\"${SERVICE_INSTANCE_ID[$service]}\"]" | jq -r '.TotalCount')" == "1" ]]
  assert_trustee_status "$STATE_DIR" "codex,openclaw" "after-fail-closed-destroy"
  record "- destroy failed closed while Trustee was unavailable; local state and ECS were unchanged."
}

destroy_service_retry() {
  local service="$1"
  local attempt
  for attempt in 1 2 3; do
    if ca_run "$STATE_DIR" destroy "$service"; then
      return 0
    fi
    record "- destroy $service attempt $attempt failed; retrying the idempotent lifecycle."
    sleep 30
  done
  echo "failed to destroy $service after three attempts" >&2
  return 1
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/trustee-duo-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  OPENCLAW_INIT_OUTPUT_DIR="$(absolute_dir "$WORK_DIR/openclaw-init")"
  CODEX_INIT_OUTPUT_DIR="$(absolute_dir "$WORK_DIR/codex-init")"
  OPENCLAW_DIR="$OPENCLAW_INIT_OUTPUT_DIR/openclaw"
  CODEX_DIR="$CODEX_INIT_OUTPUT_DIR/codex"
  OPENCLAW_DISK_GB="${E2E_TRUSTEE_OPENCLAW_DISK_GB:-60}"
  CODEX_DISK_GB="${E2E_TRUSTEE_CODEX_DISK_GB:-60}"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
  CHAT_ATTEMPTS="${E2E_CHAT_ATTEMPTS:-3}"
  OPENCLAW_CHAT_MESSAGE="${E2E_OPENCLAW_CHAT_MESSAGE:-请只回复 CA_TRUSTEE_OPENCLAW_OK，不要输出其他内容。}"
  OPENCLAW_CHAT_EXPECT="${E2E_OPENCLAW_CHAT_EXPECT:-CA_TRUSTEE_OPENCLAW_OK}"
  CODEX_CHAT_MESSAGE="${E2E_CODEX_CHAT_MESSAGE:-Reply with CA_TRUSTEE_CODEX_OK and no other text.}"
  CODEX_CHAT_EXPECT="${E2E_CODEX_CHAT_EXPECT:-CA_TRUSTEE_CODEX_OK}"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen3.8-max}"
  export CA_DAEMON_STATUS_WAIT_SEC="${CA_DAEMON_STATUS_WAIT_SEC:-1200}"

  validate_modes
  [[ "$REFERENCE_VALUES" == "rekor" ]] || {
    echo "trustee-duo requires E2E_REFERENCE_VALUES=rekor" >&2
    return 2
  }
  require_cmd aliyun
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd node
  require_cmd openclaw
  require_cmd openssl
  require_cmd python3.11
  require_cmd scp
  require_cmd setsid
  require_cmd ssh
  require_cmd ssh-keygen
  require_cmd ssh-keyscan
  require_cmd terraform
  require_cmd timeout
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Trustee same-state OpenClaw + Codex E2E"
  install_exit_traps
  record "- scope: same-state dual-agent lifecycle; CMaaS covers mesh data-plane traffic."
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local allowed_cidr cosign_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  DASHSCOPE_KEY_VALUE="$(resolve_dashscope_key)"
  OPENCLAW_TOKEN="$(resolve_token)"
  CODEX_REMOTE_TOKEN_VALUE="$(openssl rand -base64 32)"
  export COSIGN_KEY="$cosign_key"
  export CA_AGENTD_BIN="${CA_AGENTD_BIN:-$ROOT_DIR/target/debug/confidential-agentd}"
  export CA_GATEWAY_BIN="${CA_GATEWAY_BIN:-$ROOT_DIR/target/debug/cai-gateway}"
  export CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"

  assert_challenge_mode "$ROOT_DIR/examples/openclaw/openclaw.yaml"
  assert_challenge_mode "$ROOT_DIR/examples/codex/codex.yaml"
  prepare_openclaw_fixture
  prepare_codex_fixture
  assert_challenge_mode "$OPENCLAW_DIR/openclaw.yaml"
  assert_challenge_mode "$CODEX_DIR/codex.yaml"
  switch_generated_spec_to_trustee "$OPENCLAW_DIR/openclaw.yaml"
  switch_generated_spec_to_trustee "$CODEX_DIR/codex.yaml"
  validate_specs "$STATE_DIR" "$OPENCLAW_DIR/openclaw.yaml" "$CODEX_DIR/codex.yaml"

  register_destroy_target "$STATE_DIR" openclaw
  register_destroy_target "$STATE_DIR" codex

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$OPENCLAW_DIR/openclaw.yaml"
    ca_run "$STATE_DIR" build --spec "$CODEX_DIR/codex.yaml"
  fi
  record_manifest_variants "$STATE_DIR" openclaw
  record_manifest_variants "$STATE_DIR" codex
  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  provision_trustee_server "$allowed_cidr"
  configure_and_adopt_trustee "$STATE_DIR"
  trustee_start_diagnostics

  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" ]]; then
    echo "trustee-duo does not support E2E_SKIP_DEPLOY=1 because it validates the full lifecycle" >&2
    return 2
  fi

  # Deployment includes upload and ECS image import, which can exceed 20 minutes.
  # Keep the separate application readiness checks below at their existing budget.
  local deploy_timeout="${E2E_TRUSTEE_DEPLOY_TIMEOUT:-3600}"
  deploy_with_trustee_access "$STATE_DIR" openclaw "$OPENCLAW_DIR/openclaw.yaml" "$deploy_timeout"
  wait_for_status_service_ready "$STATE_DIR" openclaw 1200
  capture_service_cloud openclaw
  assert_trustee_status "$STATE_DIR" "openclaw" "after-openclaw-deploy"
  trustee_capture_diagnostics "after-openclaw-deploy"
  trustee_start_diagnostics
  local openclaw_generation_one
  openclaw_generation_one="$(state_value "$STATE_DIR" openclaw mesh_generation)"

  deploy_with_trustee_access "$STATE_DIR" codex "$CODEX_DIR/codex.yaml" "$deploy_timeout"
  capture_service_cloud codex
  assert_independent_service_infra openclaw codex
  assert_trustee_security_group
  assert_trustee_status "$STATE_DIR" "codex,openclaw" "after-duo-deploy"
  wait_for_local_lifecycle_status "codex,openclaw" "" "duo-active" 1200
  local openclaw_generation_two codex_generation_two
  openclaw_generation_two="$(state_value "$STATE_DIR" openclaw mesh_generation)"
  codex_generation_two="$(state_value "$STATE_DIR" codex mesh_generation)"
  ((openclaw_generation_two > openclaw_generation_one))
  [[ "$openclaw_generation_two" == "$codex_generation_two" ]]
  assert_trustee_guest_tng_28 openclaw
  assert_trustee_guest_tng_28 codex

  run_openclaw_conversation initial
  run_codex_conversation
  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report-duo.json" "openclaw,codex"
  cleanup_connects

  ca_run "$STATE_DIR" trustee sync
  wait_for_status_service_ready "$STATE_DIR" openclaw 600
  wait_for_status_service_ready "$STATE_DIR" codex 600
  assert_trustee_status "$STATE_DIR" "codex,openclaw" "after-idempotent-sync"

  reboot_openclaw_without_cli_sync
  assert_trustee_status "$STATE_DIR" "codex,openclaw" "after-openclaw-reboot"
  run_openclaw_conversation after-reboot
  cleanup_connects

  assert_destroy_fails_closed_while_trustee_down codex
  local policy_before_codex_destroy openclaw_generation_before_codex_destroy
  policy_before_codex_destroy="$(jq -er '.resource_policy_sha256' "$WORK_DIR/trustee-status-after-fail-closed-destroy.json")"
  openclaw_generation_before_codex_destroy="$(state_value "$STATE_DIR" openclaw mesh_generation)"
  destroy_service_retry codex
  trustee_remove_service_access codex
  assert_service_cloud_gone codex 600
  assert_trustee_status "$STATE_DIR" "openclaw" "after-codex-destroy"
  wait_for_local_lifecycle_status "openclaw" "codex" "codex-deleted" 600
  local policy_after_codex_destroy openclaw_generation_after_codex
  policy_after_codex_destroy="$(jq -er '.resource_policy_sha256' "$WORK_DIR/trustee-status-after-codex-destroy.json")"
  [[ "$policy_after_codex_destroy" != "$policy_before_codex_destroy" ]]
  openclaw_generation_after_codex="$(state_value "$STATE_DIR" openclaw mesh_generation)"
  ((openclaw_generation_after_codex > openclaw_generation_before_codex_destroy))
  run_openclaw_conversation survivor
  cleanup_connects

  destroy_service_retry openclaw
  trustee_remove_service_access openclaw
  assert_service_cloud_gone openclaw 600
  assert_trustee_status "$STATE_DIR" "" "after-all-services-destroyed"
  assert_local_lifecycle_status "" "codex,openclaw" "all-services-deleted"
  ca_run "$STATE_DIR" trustee prune

  E2E_DESTROY_TARGETS=()
  trustee_capture_diagnostics "complete" || record "- WARNING: final Trustee diagnostics capture failed."
  destroy_trustee_infra
  assert_trustee_cloud_zero 600
  record "- checked-in and init-generated defaults remained challenge; only disposable copies used Trustee."
}
