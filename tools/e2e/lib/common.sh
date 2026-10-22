#!/usr/bin/env bash

: "${ROOT_DIR:?ROOT_DIR must be set before sourcing common.sh}"
source "$ROOT_DIR/common/software-versions.sh"
export PATH="${CA_TOOLS_DIR:-/usr/libexec/cagent/bin}:$PATH"
# The normal Shelter RPM keeps its Terraform CLI alongside its other tools.
if [[ -x /usr/libexec/shelter/bin/terraform ]]; then
  export PATH="$PATH:/usr/libexec/shelter/bin"
fi

E2E_RUN_ID="${E2E_RUN_ID:-$(date +%Y%m%d%H%M%S)-$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')}"
CA_BIN="${CA_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
TOOLS_DIR="${CA_TOOLS_DIR:-/usr/libexec/cagent/bin}"
REFERENCE_VALUES="${E2E_REFERENCE_VALUES:-rekor}"
REGION="${E2E_REGION:-cn-beijing}"

default_tdx_zone_id() {
  case "$1" in
    cn-hongkong) printf '%s\n' "cn-hongkong-d" ;;
    cn-beijing) printf '%s\n' "cn-beijing-i" ;;
    *) printf '%s\n' "cn-beijing-i" ;;
  esac
}

default_tdx_instance_type() {
  case "$1" in
    cn-hongkong) printf '%s\n' "ecs.g8i.xlarge" ;;
    cn-beijing) printf '%s\n' "ecs.g9i.xlarge" ;;
    *) printf '%s\n' "ecs.g8i.xlarge" ;;
  esac
}

ZONE_ID="${E2E_ZONE_ID:-$(default_tdx_zone_id "$REGION")}"
DEFAULT_INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-$(default_tdx_instance_type "$REGION")}"
SLSA_GENERATOR="${E2E_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
DESTROY_ON_SUCCESS="${E2E_DESTROY_ON_SUCCESS:-1}"
DESTROY_ON_FAILURE="${E2E_DESTROY_ON_FAILURE:-1}"
SHELTER_DIR="${E2E_SHELTER_DIR:-/root/shelter-rs}"

STEP_LOG="${STEP_LOG:-}"
E2E_EXIT_CLEANUP_STARTED=0
E2E_DEPLOY_ATTEMPTED=0
E2E_CONNECT_PIDS=()
E2E_CONNECT_READY_FILES=()
E2E_DESTROY_TARGETS=()

log() {
  printf '[e2e:%s] %s\n' "${CASE_NAME:-unknown}" "$*"
}

cmd_string() {
  local out="" arg
  for arg in "$@"; do
    printf -v out '%s%q ' "$out" "$arg"
  done
  printf '%s' "${out% }"
}

record() {
  [[ -n "$STEP_LOG" ]] || return 0
  printf '%s\n' "$*" >>"$STEP_LOG"
}

record_cmd() {
  record ""
  record '```bash'
  record "$*"
  record '```'
}

record_file_as_block() {
  local title="$1"
  local path="$2"
  local lang="${3:-text}"
  [[ -f "$path" ]] || return 0
  record ""
  record "$title"
  record "\`\`\`$lang"
  sed -E \
    -e 's/[[:cntrl:]]\[[0-9;]*m//g' \
    -e 's/(^|[[:space:]])--token "[^"]+"/\1--token "<redacted>"/g' \
    -e 's/^([[:space:]]*token:[[:space:]]*).+$/\1<redacted>/g' \
    -e 's/token: "[^"]+"/token: "<redacted>"/g' \
    -e 's/"apiKey": "[^"]+"/"apiKey": "<redacted>"/g' \
    -e 's/"ANTHROPIC_AUTH_TOKEN": "[^"]+"/"ANTHROPIC_AUTH_TOKEN": "<redacted>"/g' \
    -e 's/^(ANTHROPIC_AUTH_TOKEN=).+$/\1<redacted>/g' \
    -e 's/^(DASHSCOPE_API_KEY=).+$/\1<redacted>/g' \
    -e 's/^(API_SERVER_KEY=).+$/\1<redacted>/g' \
    -e 's/"DASHSCOPE_API_KEY": "[^"]+"/"DASHSCOPE_API_KEY": "<redacted>"/g' \
    -e 's/"API_SERVER_KEY": "[^"]+"/"API_SERVER_KEY": "<redacted>"/g' \
    -e 's/^(OPENAI_API_KEY=).+$/\1<redacted>/g' \
    -e 's/^(CODEX_REMOTE_TOKEN=).+$/\1<redacted>/g' \
    -e 's/(CODEX_REMOTE_TOKEN=)[^[:space:]]+/\1<redacted>/g' \
    -e 's/"authorization": "Bearer [^"]+"/"authorization": "Bearer <redacted>"/Ig' \
    -e 's/(Authorization: Bearer )[[:graph:]]+/\1<redacted>/Ig' \
    -e 's/"clientSecret": "[^"]+"/"clientSecret": "<redacted>"/g' \
    -e 's/"token": "[^"]+"/"token": "<redacted>"/g' \
    -e 's/"ear_jwt": "[^"]+"/"ear_jwt": "<redacted>"/g' \
    "$path" >>"$STEP_LOG"
  record '```'
}

require_cmd() {
  # Installed-RPM runs execute prebuilt binaries; the build helpers validate
  # those executables separately and never invoke Cargo in this mode.
  if [[ "$1" == cargo && "${E2E_SKIP_CARGO_BUILD:-0}" == 1 ]]; then
    return
  fi
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

dockerhub_mirror_image() {
  local image="$1"
  local mirror="${CA_DOCKERHUB_MIRROR:-docker.1ms.run}"
  local first rest path last
  [[ -n "$image" && "$image" != "$mirror/"* ]] || return 1
  if [[ "$image" == */* ]]; then
      first="${image%%/*}"
      rest="${image#*/}"
      case "$first" in
      docker.io|index.docker.io|registry-1.docker.io)
        path="$rest"
        [[ "$path" == */* ]] || path="library/$path"
        ;;
      *.*|*:*|localhost) return 1 ;;
      *) path="$image" ;;
    esac
  else
    path="library/$image"
  fi
  if [[ "$path" != *@* ]]; then
    last="${path##*/}"
    [[ "$last" == *:* ]] || path="$path:latest"
  fi
  printf '%s/%s\n' "${mirror%/}" "$path"
}

dockerhub_primary_image() {
  local image="$1"
  local first
  [[ -n "$image" ]] || return 1
  if [[ "$image" == */* ]]; then
    first="${image%%/*}"
    case "$first" in
      docker.io|index.docker.io|registry-1.docker.io|*.*|*:*|localhost)
        printf '%s\n' "$image"
        ;;
      *)
        printf 'docker.io/%s\n' "$image"
        ;;
    esac
  else
    printf 'docker.io/library/%s\n' "$image"
  fi
}

run_with_optional_timeout() {
  local limit="$1"
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$limit" "$@"
  else
    "$@"
  fi
}

container_pull_with_fallback() {
  local runtime="$1"
  local image="$2"
  local stdout="$3"
  local stderr="$4"
  local primary
  local mirror
  local timeout_s="${CA_CONTAINER_PULL_TIMEOUT:-180s}"

  primary="$(dockerhub_primary_image "$image")"
  : >"$stdout"
  : >"$stderr"
  if [[ "$primary" != "$image" ]]; then
    printf 'normalized DockerHub image %s to %s\n' "$image" "$primary" >>"$stderr"
  fi
  if run_with_optional_timeout "$timeout_s" "$runtime" pull "$primary" >>"$stdout" 2>>"$stderr"; then
    printf '%s\n' "$primary"
    return 0
  fi
  mirror="$(dockerhub_mirror_image "$primary" || true)"
  [[ -n "$mirror" ]] || return 1
  {
    printf '\nprimary pull failed for %s; retrying via %s\n' "$primary" "$mirror"
  } >>"$stderr"
  if run_with_optional_timeout "$timeout_s" "$runtime" pull "$mirror" >>"$stdout" 2>>"$stderr"; then
    printf '%s\n' "$mirror"
    return 0
  fi
  return 1
}

validate_modes() {
  case "$REFERENCE_VALUES" in
    sample | rekor) ;;
    *) echo "E2E_REFERENCE_VALUES must be sample or rekor, got '$REFERENCE_VALUES'" >&2; exit 2 ;;
  esac
}

absolute_dir() {
  local dir="$1"
  mkdir -p "$dir"
  (cd "$dir" && pwd -P)
}

absolute_existing_path() {
  local file="$1"
  [[ -e "$file" ]] || {
    echo "path does not exist: $file" >&2
    exit 2
  }
  local parent
  parent="$(dirname "$file")"
  printf '%s/%s' "$(cd "$parent" && pwd -P)" "$(basename "$file")"
}

init_step_log() {
  local title="$1"
  mkdir -p "$WORK_DIR"
  STEP_LOG="$WORK_DIR/e2e-steps.md"
  {
    printf '# %s\n\n' "$title"
    printf '%s\n' "- work_dir: \`$WORK_DIR\`"
    printf '%s\n' "- state_dir: \`${STATE_DIR:-multi-state}\`"
    printf '%s\n' "- tools_dir: \`$TOOLS_DIR\`"
    printf '%s\n' "- reference_values: \`$REFERENCE_VALUES\`"
    printf '%s\n' "- region: \`$REGION\`"
    printf '%s\n' "- zone_id: \`$ZONE_ID\`"
    printf '%s\n' "- instance_type: \`${INSTANCE_TYPE:-$DEFAULT_INSTANCE_TYPE}\`"
    printf '%s\n' "- proxy: inherited from the outer command; this script does not unset proxy variables"
  } >"$STEP_LOG"
}

install_exit_traps() {
  E2E_TRAP_OWNER_BASHPID="$BASHPID"
  trap 'finish_e2e "$?"' EXIT ERR
  trap 'finish_e2e 130' INT
  trap 'finish_e2e 143' TERM
}

register_destroy_target() {
  local state_dir="$1"
  local service="$2"
  E2E_DESTROY_TARGETS+=("$state_dir|$service")
  if [[ -n "${WORK_DIR:-}" ]]; then
    python3.11 - "$WORK_DIR/destroy-targets.json" "$state_dir" "$service" <<'PYREGISTRY'
import json, sys
from pathlib import Path
path, state, service = sys.argv[1:]
path = Path(path)
path.parent.mkdir(parents=True, exist_ok=True)
values = json.loads(path.read_text()) if path.exists() else []
target = {"state_dir": state, "service": service}
if target not in values:
    values.append(target)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(values, indent=2) + "\n")
    temporary.replace(path)
PYREGISTRY
  fi

}

cleanup_connects() {
  local ready_file
  for ready_file in "${E2E_CONNECT_READY_FILES[@]}"; do
    cleanup_connect_ready "$ready_file"
  done
  # Connect helpers are commonly called through $(...), whose array updates
  # do not reach the parent shell. Their per-attempt ready files survive.
  if [[ -n "${WORK_DIR:-}" ]]; then
    for ready_file in "$WORK_DIR"/*-connect-attempt-*-ready.json; do
      cleanup_connect_ready "$ready_file"
    done
  fi
  E2E_CONNECT_READY_FILES=()

  local pid
  for pid in "${E2E_CONNECT_PIDS[@]}"; do
    [[ -n "$pid" ]] || continue
    kill -- "-$pid" >/dev/null 2>&1 || kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
    kill -9 -- "-$pid" >/dev/null 2>&1 || kill -9 "$pid" >/dev/null 2>&1 || true
  done
  E2E_CONNECT_PIDS=()
}

destroy_registered_resources() {
  local reason="$1"
  local target state_dir service phase
  local cleanup_status=0
  for target in "${E2E_DESTROY_TARGETS[@]}"; do
    state_dir="${target%%|*}"
    service="${target#*|}"
    if [[ ! -f "$state_dir/services/$service/manifest.json" ]]; then
      record "- destroy $service: skipped; no manifest in $state_dir."
      continue
    fi
    phase="$(state_value "$state_dir" "$service" phase 2>/dev/null || true)"
    if [[ "$phase" == "deleted" ]]; then
      record "- destroy $service: skipped; phase=deleted (formal destroy already completed)."
      continue
    fi
    log "destroying $service ($reason)"
    ca_run "$state_dir" destroy "$service" || cleanup_status=1
  done
  return "$cleanup_status"
}

finish_e2e() {
  local status="$1"
  # ERR traps are inherited by command substitutions under `set -E`.
  # Only the shell that installed the traps owns the cloud resources.
  if [[ -n "${E2E_TRAP_OWNER_BASHPID:-}" && "$BASHPID" != "$E2E_TRAP_OWNER_BASHPID" ]]; then
    trap - EXIT ERR
    exit "$status"
  fi
  local cleanup_snapshots=0
  if [[ "$E2E_EXIT_CLEANUP_STARTED" == "1" ]]; then
    exit "$status"
  fi
  E2E_EXIT_CLEANUP_STARTED=1
  cleanup_connects || true
  if declare -f case_cleanup >/dev/null 2>&1; then
    case_cleanup "$status" || status=1
  fi
  if [[ "$E2E_DEPLOY_ATTEMPTED" == "1" ]]; then
    if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
      cleanup_snapshots=1
      destroy_registered_resources success || status=1
    elif [[ "$status" != "0" && "$DESTROY_ON_FAILURE" == "1" ]]; then
      cleanup_snapshots=1
      destroy_registered_resources failure || status=1
    fi
  fi
  if [[ "$cleanup_snapshots" == "1" && -n "${WORK_DIR:-}" ]]; then
    python3.11 "$ROOT_DIR/tools/e2e/cleanup_snapshots.py" cleanup \
      "$WORK_DIR/cloud-resource-ledger.json" "$REGION" || status=1
  fi
  if declare -f case_post_destroy_cleanup >/dev/null 2>&1; then
    case_post_destroy_cleanup "$status" || status=1
  fi
  if [[ -n "${WORK_DIR:-}" ]]; then
    python3.11 - "$WORK_DIR/case-result.json" "$status" "${E2E_SKIP_DEPLOY:-0}" "${DESTROY_ON_SUCCESS:-1}" "${DESTROY_ON_FAILURE:-1}" <<'PYRESULT' || status=1
import json, sys
from pathlib import Path
path, status, skipped, success_cleanup, failure_cleanup = sys.argv[1:]
status = int(status)
result = {"schema_version": "confidential-agent/e2e-case/v1", "passed": status == 0 and skipped != "1",
          "cleanup_complete": success_cleanup == "1" and failure_cleanup == "1" and status == 0,
          "exit_code": status, "cloud_skipped": skipped == "1"}
Path(path).write_text(json.dumps(result, indent=2) + "\n")
PYRESULT
  fi
  record ""
  if [[ "$status" == "0" ]]; then
    record "Result: PASS"
    log "completed; step log: $STEP_LOG"
  else
    record "Result: FAIL ($status)"
    log "failed; step log: $STEP_LOG"
  fi
  exit "$status"
}

ca_args() {
  local state_dir="$1"
  shift
  printf '%s\0' "$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" "$@"
}

# Keep only resource identifiers, never the secret-bearing Terraform state.
record_destroy_ledger() {
  python3.11 - "$1/services/$2" "$WORK_DIR/cloud-resource-ledger.json" <<'PYLEDGER'
import json
import sys
from pathlib import Path
service, output = map(Path, sys.argv[1:])
entries = json.loads(output.read_text()) if output.exists() else []
for state in service.rglob("*.tfstate"):
    data = json.loads(state.read_text())
    for resource in data.get("resources", []):
        kind = resource.get("type", "")
        if resource.get("mode") != "managed" or not kind.startswith("alicloud_"):
            continue
        for instance in resource.get("instances", []):
            attrs = instance.get("attributes", {})
            item = {"type": kind, "id": attrs.get("id"), "service": service.name}
            for key in ["region", "bucket", "key", "image_id", "system_disk_id", "vpc_id"]:
                if attrs.get(key):
                    item[key] = attrs[key]
            if item not in entries:
                entries.append(item)
local_state = service / "state.json"
if local_state.exists():
    data = json.loads(local_state.read_text())
    for published in data.get("build", {}).get("published", {}).values():
        for kind, field in [("alicloud_image", "image_id"), ("alicloud_oss_bucket", "bucket")]:
            if published.get(field):
                item = {"type": kind, "id": published[field], "service": service.name}
                if item not in entries:
                    entries.append(item)
output.write_text(json.dumps(entries, indent=2) + "\n")
PYLEDGER
  python3.11 "$ROOT_DIR/tools/e2e/cleanup_snapshots.py" record \
    "$WORK_DIR/cloud-resource-ledger.json" "$REGION"
}

ca_run() {
  local state_dir="$1"
  shift
  if [[ "${1:-}" == "destroy" && -n "${2:-}" ]]; then
    record_destroy_ledger "$state_dir" "$2" || return
  fi
  local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" "$@")
  record_cmd "$(cmd_string "${cmd[@]}")"
  E2E_WORK_DIR="$WORK_DIR" python3.11 "$ROOT_DIR/tools/e2e/command_runner.py" "${1:-command}" -- "${cmd[@]}"
}

ca_capture() {
  local state_dir="$1"
  local stdout_path="$2"
  local stderr_path="$3"
  shift 3
  local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" "$@")
  record_cmd "$(cmd_string "${cmd[@]}")"
  E2E_WORK_DIR="$WORK_DIR" python3.11 "$ROOT_DIR/tools/e2e/command_runner.py" "${1:-command}" -- "${cmd[@]}" >"$stdout_path" 2>"$stderr_path"
}

redacted_cmd_string() {
  local out="" arg redact_next=0
  for arg in "$@"; do
    if [[ "$redact_next" == "1" ]]; then
      printf -v out '%s%s ' "$out" "'<redacted>'"
      redact_next=0
      continue
    fi
    case "$arg" in
      --dashscope-api-key|--gateway-token|--hermes-api-server-key|--codex-app-server-token)
        printf -v out '%s%q ' "$out" "$arg"
        redact_next=1
        ;;
      *)
        printf -v out '%s%q ' "$out" "$arg"
        ;;
    esac
  done
  printf '%s' "${out% }"
}

ca_init_capture() {
  local state_dir="$1"
  local stdout_path="$2"
  local stderr_path="$3"
  shift 3
  local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" init "$@")
  record_cmd "$(redacted_cmd_string "${cmd[@]}")"
  "${cmd[@]}" >"$stdout_path" 2>"$stderr_path"
}

assert_init_script_extends_example() {
  local generated="$1"
  local example="$2"
  shift 2
  python3.11 - "$generated" "$example" "$@" <<'PY'
from pathlib import Path
import sys

generated = Path(sys.argv[1])
example = Path(sys.argv[2])
export_keys = tuple(sys.argv[3:])

def strip_injected_exports(text: str) -> str:
    lines = []
    prefixes = tuple(f"export {key}=" for key in export_keys)
    for line in text.splitlines(keepends=True):
        if prefixes and line.startswith(prefixes):
            continue
        lines.append(line)
    return "".join(lines)

actual = strip_injected_exports(generated.read_text(encoding="utf-8"))
expected = strip_injected_exports(example.read_text(encoding="utf-8"))
if actual != expected:
    print(f"{generated} does not match example script {example} after removing injected exports", file=sys.stderr)
    raise SystemExit(1)
PY
  record "- Init install script \`$generated\` extends example script \`$example\`."
}

init_common_args() {
  local output_dir="$1"
  local disk_gb="$2"
  local args=(
    --non-interactive
    --force
    --output-dir "$output_dir"
    --region "$REGION"
    --zone-id "$ZONE_ID"
    --instance-type "${INSTANCE_TYPE:-$DEFAULT_INSTANCE_TYPE}"
    --disk-gb "$disk_gb"
    --reference-values "$REFERENCE_VALUES"
    --slsa-generator "$SLSA_GENERATOR"
  )
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    : "${COSIGN_KEY:?COSIGN_KEY must be set before init_common_args when E2E_REFERENCE_VALUES=rekor}"
    args+=(--cosign-key "$COSIGN_KEY")
  fi
  printf '%s\0' "${args[@]}"
}

use_aliyun_cli_profile() {
  command -v aliyun >/dev/null 2>&1 || return 1
  local profile="${ALICLOUD_PROFILE:-${ALIBABA_CLOUD_PROFILE:-}}"
  if [[ -z "$profile" ]]; then
    local profile_line
    profile_line="$(aliyun configure get profile 2>/dev/null || true)"
    profile_line="${profile_line%%$'\n'*}"
    [[ "$profile_line" == profile=* ]] || return 1
    profile="${profile_line#profile=}"
    profile="${profile%$'\r'}"
  fi
  [[ -n "$profile" ]] || return 1
  aliyun --profile "$profile" sts GetCallerIdentity >/dev/null 2>&1 || return 1
  export ALICLOUD_PROFILE="$profile"
}

require_aliyun_credentials() {
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    return
  fi
  if [[ -n "${ALIBABA_CLOUD_ACCESS_KEY_ID:-}" && -n "${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}" ]]; then
    return
  fi
  if use_aliyun_cli_profile; then
    return
  fi
  echo "Aliyun credentials are required before E2E build/deploy." >&2
  exit 2
}

require_bailian_credentials() {
  if [[ -n "${DASHSCOPE_API_KEY:-}" || -n "${BAILIAN_API_KEY:-}" ]]; then
    return
  fi
  echo "DASHSCOPE_API_KEY or BAILIAN_API_KEY is required for this case." >&2
  exit 2
}

resolve_dashscope_key() {
  if [[ -n "${DASHSCOPE_API_KEY:-}" ]]; then
    printf '%s' "$DASHSCOPE_API_KEY"
  elif [[ -n "${BAILIAN_API_KEY:-}" ]]; then
    printf '%s' "$BAILIAN_API_KEY"
  fi
}

resolve_allowed_cidr() {
  if [[ -n "${E2E_ALLOWED_CIDR:-}" ]]; then
    printf '%s' "$E2E_ALLOWED_CIDR"
    return
  fi
  local ip
  ip="$(curl -fsSL https://ipinfo.io/ip 2>/dev/null || curl -fsSL https://api.ipify.org)"
  IFS=. read -r a b c _ <<<"$ip"
  if [[ -n "${a:-}" && -n "${b:-}" && -n "${c:-}" ]]; then
    printf '%s.%s.%s.0/24' "$a" "$b" "$c"
  else
    printf '%s/32' "$ip"
  fi
}

resolve_token() {
  if [[ -n "${OPENCLAW_GATEWAY_TOKEN:-}" ]]; then
    printf '%s' "$OPENCLAW_GATEWAY_TOKEN"
  else
    openssl rand -hex 20
  fi
}

resolve_cosign_key() {
  if [[ "$REFERENCE_VALUES" != "rekor" ]]; then
    return
  fi
  if [[ -n "${E2E_COSIGN_KEY:-}" ]]; then
    absolute_existing_path "$E2E_COSIGN_KEY"
    return
  fi
  mkdir -p "$WORK_DIR/secrets"
  local prefix="$WORK_DIR/secrets/cosign"
  if [[ ! -f "$prefix.key" ]]; then
    local tool_state="$WORK_DIR/tools-state"
    mkdir -p "$tool_state"
    local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$tool_state" key generate-cosign --output-key-prefix "$prefix")
    record_cmd "$(cmd_string "${cmd[@]}")"
    "${cmd[@]}" >/dev/null
  fi
  printf '%s' "$prefix.key"
}

resolve_shelter_rpm() {
  if [[ -n "${E2E_SHELTER_RPM:-}" ]]; then
    [[ -f "$E2E_SHELTER_RPM" ]] || {
      echo "Shelter RPM does not exist: $E2E_SHELTER_RPM" >&2
      exit 2
    }
    printf '%s\n' "$E2E_SHELTER_RPM"
    return
  fi
  find "$ROOT_DIR/hack" -maxdepth 1 -type f -name 'shelter-*.rpm' | sort -V | tail -n 1
}

install_bundled_shelter_rpm() {
  local rpm pm
  rpm="$(resolve_shelter_rpm)"
  [[ -n "$rpm" ]] || {
    echo "shelter is missing and no bundled Shelter RPM was found" >&2
    exit 2
  }
  if command -v dnf >/dev/null 2>&1; then
    pm=dnf
  elif command -v yum >/dev/null 2>&1; then
    pm=yum
  else
    echo "shelter is missing and neither dnf nor yum is available" >&2
    exit 2
  fi
  local required installed operation=install
  required="$(rpm -qp --qf '%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}' "$rpm")"
  installed="$(rpm -q --qf '%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}' shelter 2>/dev/null || true)"
  [[ "$required" != "$installed" ]] || operation=reinstall
  record_cmd "$(cmd_string "$pm" "$operation" -y "$rpm")"
  "$pm" "$operation" -y "$rpm"
  "$pm" install -y libguestfs-tools-c
}

ensure_shelter() {
  local bundled_rpm=""
  if [[ -z "${CA_SHELTER_BIN:-}" && "${E2E_SKIP_SHELTER_INSTALL:-0}" != "1" && "${E2E_USE_SOURCE_SHELTER:-0}" != "1" ]]; then
    bundled_rpm="$(resolve_shelter_rpm)"
    [[ -n "$bundled_rpm" ]] || { echo "No bundled Shelter RPM was found" >&2; exit 2; }
  fi
  export CA_SHELTER_BIN="${CA_SHELTER_BIN:-/usr/bin/shelter}"
  if [[ "${E2E_USE_SOURCE_SHELTER:-0}" == "1" ]]; then
    if [[ -x "$SHELTER_DIR/target/release/shelter" ]]; then
      export CA_SHELTER_BIN="$SHELTER_DIR/target/release/shelter"
    elif [[ -x "$SHELTER_DIR/target/debug/shelter" ]]; then
      export CA_SHELTER_BIN="$SHELTER_DIR/target/debug/shelter"
    fi
  fi
  if [[ "${E2E_SKIP_SHELTER_INSTALL:-0}" != "1" && "${E2E_USE_SOURCE_SHELTER:-0}" != "1" ]] &&
     { ! "$ROOT_DIR/hack/check-shelter.sh" "$CA_SHELTER_BIN" >/dev/null 2>&1 ||
       { [[ -n "$bundled_rpm" ]] && ! command -v virt-customize >/dev/null 2>&1; } ||
       { [[ -n "$bundled_rpm" ]] && ! "$ROOT_DIR/hack/check-shelter-rpm.sh" "$CA_SHELTER_BIN" "$bundled_rpm" >/dev/null 2>&1; }; }; then
    install_bundled_shelter_rpm
  fi
  if [[ -n "$bundled_rpm" ]]; then
    "$ROOT_DIR/hack/check-shelter-rpm.sh" "$CA_SHELTER_BIN" "$bundled_rpm"
  fi
  command -v "$CA_SHELTER_BIN" >/dev/null 2>&1 || {
    echo "Shelter command '$CA_SHELTER_BIN' is not available" >&2
    exit 2
  }
  "$ROOT_DIR/hack/check-shelter.sh" "$CA_SHELTER_BIN"
  sha256sum "$(command -v "$CA_SHELTER_BIN")" > "$WORK_DIR/shelter-sha256.txt"
  record_file_as_block "Shelter binary SHA256:" "$WORK_DIR/shelter-sha256.txt" text
  record_cmd "$CA_SHELTER_BIN --version"
  "$CA_SHELTER_BIN" --version | tee "$WORK_DIR/shelter-version.txt"
  record_file_as_block "Shelter version:" "$WORK_DIR/shelter-version.txt" text
}

verify_slsa_generator() {
  if [[ "$REFERENCE_VALUES" != "rekor" ]]; then
    return
  fi
  [[ -x "$SLSA_GENERATOR" ]] || {
    echo "SLSA generator '$SLSA_GENERATOR' is not executable" >&2
    exit 2
  }
  record_cmd "$SLSA_GENERATOR --help"
  "$SLSA_GENERATOR" --help >/dev/null
}

build_host_binaries() {
  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" == "1" ]]; then
    [[ -x "$CA_BIN" ]] || {
      echo "CA_BIN '$CA_BIN' is not executable" >&2
      exit 2
    }
    return
  fi
  record_cmd "cargo build $*"
  (cd "$ROOT_DIR" && cargo build "$@")
}

verify_cai_pep_binary() {
  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    record_cmd "cargo clean -p cai-pep && cargo build -p cai-pep"
    (cd "$ROOT_DIR" && cargo clean -p cai-pep && cargo build -p cai-pep)
  fi

  local bin="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"
  [[ -x "$bin" ]] || {
    echo "cai-pep '$bin' is not executable; set CA_PEP_BIN or build it" >&2
    exit 2
  }

  local policy_source="$ROOT_DIR/examples/openclaw/files/cai-pep-default-policy.json"
  [[ -f "$policy_source" ]] || {
    echo "missing cai-pep default policy: $policy_source" >&2
    exit 2
  }

  local tmp_dir socket stdout stderr pid ok
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ca-pep-verify.XXXXXX")"
  socket="$tmp_dir/pep.sock"
  stdout="$tmp_dir/stdout.log"
  stderr="$tmp_dir/stderr.log"
  cp "$policy_source" "$tmp_dir/policy.json"

  "$bin" serve --config "$tmp_dir/policy.json" --socket "$socket" >"$stdout" 2>"$stderr" &
  pid=$!
  ok=0
  for _ in $(seq 1 20); do
    if [[ -S "$socket" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      ok=1
      break
    fi
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done

  kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true

  if [[ "$ok" != "1" ]]; then
    record_file_as_block "cai-pep verify stdout:" "$stdout" text
    record_file_as_block "cai-pep verify stderr:" "$stderr" text
    rm -rf "$tmp_dir"
    echo "target/debug/cai-pep did not stay running and create a Unix socket; refusing to package it" >&2
    exit 2
  fi

  rm -rf "$tmp_dir"
  record "Verified target/debug/cai-pep starts and creates a Unix socket before packaging."
}

render_case() {
  record_cmd "python3.11 tools/e2e/render_case.py --case $CASE_NAME --work-dir $(printf '%q' "$WORK_DIR")"
  ROOT_DIR="$ROOT_DIR" \
  WORK_DIR="$WORK_DIR" \
  REFERENCE_VALUES="$REFERENCE_VALUES" \
  SLSA_GENERATOR="$SLSA_GENERATOR" \
  REGION="$REGION" \
  ZONE_ID="$ZONE_ID" \
  INSTANCE_TYPE="${INSTANCE_TYPE:-}" \
  DISK_GB="${DISK_GB:-}" \
  CAI_PEP="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}" \
  DASHSCOPE_KEY="${DASHSCOPE_KEY:-}" \
  python3.11 "$ROOT_DIR/tools/e2e/render_case.py" --case "$CASE_NAME" --work-dir "$WORK_DIR"
}

validate_specs() {
  local state_dir="$1"
  shift
  local spec
  for spec in "$@"; do
    ca_run "$state_dir" spec validate --spec "$spec"
  done
}

ensure_operator_peering() {
  local state_dir="$1"
  local label="$2"
  local cidr="$3"
  local show_out="$WORK_DIR/peering-$label.txt"
  if ca_capture "$state_dir" "$show_out" "$WORK_DIR/peering-$label.err" peering show "$label"; then
    if grep -Fxq "cidr: $cidr" "$show_out"; then
      record "- peering $label: already present for \`$cidr\`."
      return 0
    fi
    ca_run "$state_dir" peering remove "$label"
  fi
  ca_run "$state_dir" peering add --role operator --cidr "$cidr" --label "$label"
}

record_manifest_variants() {
  local state_dir="$1"
  local service="$2"
  local manifest="$state_dir/services/$service/manifest.json"
  [[ -f "$manifest" ]] || return 0
  local summary="$WORK_DIR/$service-variants.txt"
  python3.11 - "$manifest" >"$summary" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    manifest = json.load(f)
print(f"selected_build_id={manifest.get('shelter_build_id', '')}")
variants = manifest.get("variants") or {}
print("variants=" + ",".join(sorted(variants)))
for name in sorted(variants):
    entry = variants[name] or {}
    print(f"{name}.build_id={entry.get('shelter_build_id', '')}")
PY
  record_file_as_block "$service build variants:" "$summary" text
}

state_value() {
  local state_dir="$1"
  local service="$2"
  local expr="$3"
  python3.11 - "$state_dir/services/$service/state.json" "$expr" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)
value = state
for part in sys.argv[2].split("."):
    value = value.get(part) if isinstance(value, dict) else None
print(value or "")
PY
}

wait_for_ssh() {
  local host="$1"
  local key="$2"
  local deadline=$((SECONDS + ${3:-300}))
  while (( SECONDS < deadline )); do
    if ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=10 -o BatchMode=yes root@"$host" true >/dev/null 2>&1; then
      return 0
    fi
    sleep 5
  done
  echo "timed out waiting for ssh root@$host" >&2
  return 1
}

ssh_guest() {
  local key="$1"
  local host="$2"
  shift 2
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 -o BatchMode=yes \
    -o ServerAliveInterval=15 -o ServerAliveCountMax=2 \
    root@"$host" "$@"
}

guest_wait() {
  local host="$1"
  local key="$2"
  local label="$3"
  local command="$4"
  local timeout="${5:-900}"
  local deadline=$((SECONDS + timeout))
  record_cmd "ssh -i <debug_ssh> root@$host '$command'"
  while (( SECONDS < deadline )); do
    if ssh_guest "$key" "$host" "$command" >"$WORK_DIR/guest-$label.out" 2>"$WORK_DIR/guest-$label.err"; then
      record_file_as_block "$label stdout:" "$WORK_DIR/guest-$label.out" text
      return 0
    fi
    sleep 10
  done
  record_file_as_block "$label stdout:" "$WORK_DIR/guest-$label.out" text
  record_file_as_block "$label stderr:" "$WORK_DIR/guest-$label.err" text
  echo "timed out waiting for guest condition: $label" >&2
  return 1
}

wait_for_live_status() {
  local label="$1"
  local ip="$2"
  local expected_generation="${3:-}"
  local deadline=$((SECONDS + ${4:-900}))
  while (( SECONDS < deadline )); do
    local path="$WORK_DIR/status-$label.json"
    if curl -fsS --max-time 5 "http://$ip:8088/status" -o "$path"; then
      if python3.11 - "$path" "$expected_generation" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    status = json.load(f)
expected = sys.argv[2]
ok = status.get("phase") == "running" and status.get("app_ready") is True and status.get("mesh_ready") is True
if expected:
    ok = ok and str(status.get("mesh_generation", "")) == expected
raise SystemExit(0 if ok else 1)
PY
      then
        record_file_as_block "$label live status:" "$path" json
        return 0
      fi
    fi
    sleep 5
  done
  echo "timed out waiting for $label live status" >&2
  return 1
}

wait_for_status_service_ready() {
  local state_dir="$1"
  local service="$2"
  local timeout="${3:-900}"
  local ip generation
  ip="$(state_value "$state_dir" "$service" deploy.public_ip)"
  generation="$(state_value "$state_dir" "$service" mesh_generation)"
  [[ -n "$ip" ]] || {
    echo "missing public IP for service $service" >&2
    return 1
  }
  wait_for_live_status "$service" "$ip" "$generation" "$timeout"
}

parse_connect_port() {
  local log_path="$1"
  [[ -s "$log_path" ]] || return 0
  awk '/^connect 127\.0\.0\.1:/ { split($2, a, ":"); print a[2]; exit }' "$log_path"
}

connect_ready_ports() {
  local ready_json="$1"
  [[ -s "$ready_json" ]] || return 0
  python3.11 - "$ready_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    ready = json.load(f)
for endpoint in ready.get("client_endpoints", []):
    port = endpoint.get("local_port")
    if port:
        print(port)
PY
}

cleanup_connect_ready() {
  local ready_json="$1"
  [[ -n "$ready_json" && -f "$ready_json" ]] || return 0
  "$CA_BIN" --tools-dir "$TOOLS_DIR" connect stop --ready-json "$ready_json" >/dev/null 2>&1 || true

}

cleanup_connect_pid() {
  local pid="$1"
  [[ -n "$pid" ]] || return 0
  kill -- "-$pid" >/dev/null 2>&1 || kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
}

capture_connect_diagnostics() {
  local ready_json="$1" output="$2"
  python3.11 - "$ready_json" "$output" <<'PYLOG'
import json, pathlib, sys
out = pathlib.Path(sys.argv[2])
try:
    ready = json.loads(pathlib.Path(sys.argv[1]).read_text())
    log = pathlib.Path(ready["log_file"])
    with log.open("rb") as stream:
        stream.seek(max(0, log.stat().st_size - 65536))
        text = stream.read().decode(errors="replace")
except (OSError, ValueError, KeyError):
    text = "Native tunnel diagnostics unavailable"
out.write_text(text)
out.chmod(0o600)
PYLOG
  python3.11 - "$output" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
text = p.read_text(errors="replace")
text = re.sub(r"(?i)(bearer\s+)\S+", r"\1[redacted]", text)
text = re.sub(r"eyJ[A-Za-z0-9_.-]+", "[redacted JWT]", text)
p.write_text(text)
p.chmod(0o600)
PY
  record_file_as_block "Connect process diagnostics:" "$output" text
}

start_connect_until_http_ready() {
  local state_dir="$1"
  local path_prefix="$2"
  local http_path="${3:-/openclaw}"
  local attempts="${4:-4}"
  local wait_seconds="${5:-180}"
  if (($# >= 5)); then
    shift 5
  else
    shift "$#"
  fi
  local extra_args=("$@")
  local attempt
  for attempt in $(seq 1 "$attempts"); do
    local log_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.log"
    local stdout_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.stdout"
    local stderr_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.stderr"
    local ready_json="$WORK_DIR/$path_prefix-connect-attempt-$attempt-ready.json"
    local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" connect start "${extra_args[@]}" --ready-json "$ready_json" --wait-ready "$wait_seconds" --log-file "$log_path")
    record_cmd "$(cmd_string "${cmd[@]}")"
    if ! "${cmd[@]}" >"$stdout_path" 2>"$stderr_path"; then
      record_file_as_block "$path_prefix connect start stdout:" "$stdout_path" text
      record_file_as_block "$path_prefix connect start stderr:" "$stderr_path" text
      record_file_as_block "$path_prefix connect log:" "$log_path" text
      sleep 10
      continue
    fi
    E2E_CONNECT_READY_FILES+=("$ready_json")
    record_file_as_block "$path_prefix connect start stdout:" "$stdout_path" text
    record_file_as_block "$path_prefix connect start stderr:" "$stderr_path" text
    record_file_as_block "$path_prefix connect ready:" "$ready_json" json
    record_file_as_block "$path_prefix connect log:" "$log_path" text

    local port remaining probe_started=$SECONDS deadline=$((SECONDS + wait_seconds))
    local http_error="$WORK_DIR/$path_prefix-connect-attempt-$attempt-http.stderr"
    # A bound local port does not mean the attested upstream is ready yet.
    while ((SECONDS < deadline)); do
      while IFS= read -r port; do
        remaining=$((deadline - SECONDS))
        if ((remaining <= 0)); then break; fi
        if [[ -n "$port" ]] && curl -fsS --max-time "$remaining" "http://127.0.0.1:$port$http_path" >/dev/null 2>"$http_error"; then
          record "- HTTP endpoint on local port $port became ready after $((SECONDS - probe_started)) seconds."
          printf '%s\n' "$port"
          return 0
        fi
      done < <(connect_ready_ports "$ready_json")
      sleep 1
    done
    record_file_as_block "$path_prefix HTTP readiness stderr:" "$http_error" text
    capture_connect_diagnostics "$ready_json" "$WORK_DIR/$path_prefix-connect-attempt-$attempt-process.log"
    cleanup_connect_ready "$ready_json"
    if ((attempt < attempts)); then sleep 10; fi
  done
  echo "connect did not become HTTP-ready after $attempts attempts" >&2
  return 1
}

start_connect_until_local_port_ready() {
  local state_dir="$1"
  local path_prefix="$2"
  shift 2
  local extra_args=("$@")
  local attempts="${E2E_CONNECT_ATTEMPTS:-4}"
  local wait_seconds="${E2E_CONNECT_WAIT_SECONDS:-180}"
  local attempt
  for attempt in $(seq 1 "$attempts"); do
    local log_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.log"
    local stdout_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.stdout"
    local stderr_path="$WORK_DIR/$path_prefix-connect-attempt-$attempt.stderr"
    local ready_json="$WORK_DIR/$path_prefix-connect-attempt-$attempt-ready.json"
    local cmd=("$CA_BIN" "--tools-dir" "$TOOLS_DIR" "--state-dir" "$state_dir" connect start "${extra_args[@]}" --ready-json "$ready_json" --wait-ready "$wait_seconds" --log-file "$log_path")
    record_cmd "$(cmd_string "${cmd[@]}")"
    if ! "${cmd[@]}" >"$stdout_path" 2>"$stderr_path"; then
      record_file_as_block "$path_prefix connect start stdout:" "$stdout_path" text
      record_file_as_block "$path_prefix connect start stderr:" "$stderr_path" text
      record_file_as_block "$path_prefix connect log:" "$log_path" text
      sleep 10
      continue
    fi
    E2E_CONNECT_READY_FILES+=("$ready_json")
    record_file_as_block "$path_prefix connect start stdout:" "$stdout_path" text
    record_file_as_block "$path_prefix connect start stderr:" "$stderr_path" text
    record_file_as_block "$path_prefix connect ready:" "$ready_json" json
    record_file_as_block "$path_prefix connect log:" "$log_path" text

    local port
    port="$(connect_ready_ports "$ready_json" | head -n 1 || true)"
    if [[ -n "$port" ]]; then
        printf '%s\n' "$port"
        return 0
    fi
    cleanup_connect_ready "$ready_json"
    sleep 10
  done
  echo "connect did not become local-port-ready after $attempts attempts" >&2
  return 1
}

run_openclaw_chat_probe() {
  local url="$1"
  local token="$2"
  local message="$3"
  local expect="$4"
  local output="$5"
  shift 5
  record_cmd "node tools/e2e/probes/openclaw-chat-probe.mjs --url $url --token '<redacted>' --message '<redacted>' --expect $expect"
  local probe_status=0
  if node "$ROOT_DIR/tools/e2e/probes/openclaw-chat-probe.mjs" \
    --url "$url" \
    --token "$token" \
    --message "$message" \
    --expect "$expect" \
    "$@" | tee "$output"; then
    probe_status=0
  else
    probe_status=$?
  fi
  record_file_as_block "OpenClaw chat probe:" "$output" json
  return "$probe_status"
}

run_report_probe() {
  local state_dir="$1"
  local output="$2"
  local expected_services="$3"
  local expected_a2a_peers="${4:-}"
  local reference_values="${5:-$REFERENCE_VALUES}"
  # connect can refresh mesh resources; wait for that generation to settle.
  local service
  local -a report_services
  IFS=',' read -r -a report_services <<<"$expected_services"
  for service in "${report_services[@]}"; do
    wait_for_status_service_ready "$state_dir" "$service" 600
  done
  ca_run "$state_dir" report --include-a2a --json --out "$output"
  record_file_as_block "Attestation report:" "$output" json
  python3.11 - "$output" "$expected_services" "$expected_a2a_peers" "$reference_values" <<'PY'
import json
import sys

path, services_arg, peers_arg, reference_values = sys.argv[1:5]
expected_services = [item for item in services_arg.split(",") if item]
expected_peers = [item for item in peers_arg.split(",") if item]

with open(path, encoding="utf-8") as f:
    report = json.load(f)

def fail(message):
    raise SystemExit(message)

if report.get("schema") != "confidential-agent/attestation-report/v1":
    fail(f"unexpected report schema: {report.get('schema')!r}")
if not report.get("generated_at"):
    fail("report generated_at is empty")

services = {svc.get("service_id"): svc for svc in report.get("services", [])}
missing_services = [svc for svc in expected_services if svc not in services]
if missing_services:
    fail(f"report missing services: {missing_services}")

for service_id in expected_services:
    svc = services[service_id]
    if svc.get("phase") != "active":
        fail(f"{service_id} phase is {svc.get('phase')!r}, expected active")
    collect_errors = svc.get("collect_errors") or []
    rekor_only_partial = (
        reference_values == "rekor"
        and svc.get("collect_status") == "partial"
        and collect_errors
        and all(
            isinstance(error, str)
            and error.startswith("rekor:")
            and any(marker in error for marker in ("Network Error", "timed out", "502 Bad Gateway"))
            for error in collect_errors
        )
    )
    if svc.get("collect_status") != "ok" and not rekor_only_partial:
        fail(f"{service_id} collect_status is {svc.get('collect_status')!r}: {svc.get('collect_errors')}")
    if rekor_only_partial:
        print(f"{service_id} collect_status is partial due to Rekor network errors; core attestation fields will still be checked")
    build = svc.get("build") or {}
    for field in ("build_id", "image_name", "variant", "spec_sha256", "tee", "reference_values_mode"):
        if not build.get(field):
            fail(f"{service_id} build.{field} is empty")
    tee_info = svc.get("tee_info") or {}
    if tee_info.get("status") != "ok" or tee_info.get("tee") != "tdx":
        fail(f"{service_id} tee_info is not ok/tdx: {tee_info}")
    attestation = svc.get("attestation") or {}
    if attestation.get("status") != "ok" or not attestation.get("ear_jwt") or not isinstance(attestation.get("ear_claims"), dict):
        fail(f"{service_id} attestation is incomplete")
    daemon = svc.get("daemon") or {}
    if daemon.get("status") != "ok" or daemon.get("phase") != "running":
        fail(f"{service_id} daemon is not running: {daemon}")
    if daemon.get("app_ready") is not True or daemon.get("mesh_ready") is not True:
        fail(f"{service_id} daemon readiness is not true: {daemon}")
    rekor = svc.get("rekor") or {}
    if reference_values == "rekor":
        if not rekor_only_partial and (rekor.get("status") != "found" or not rekor.get("entries")):
            fail(f"{service_id} Rekor entries were not found: {rekor}")
    elif rekor.get("status") != "not_applicable":
        fail(f"{service_id} Rekor status should be not_applicable for {reference_values}: {rekor}")

peers = {peer.get("alias") or peer.get("url"): peer for peer in report.get("a2a_peers", [])}
missing_peers = [peer for peer in expected_peers if peer not in peers]
if missing_peers:
    fail(f"report missing A2A peers: {missing_peers}")

for alias in expected_peers:
    peer = peers[alias]
    if peer.get("fetch_status") != "ok":
        fail(f"A2A peer {alias} fetch_status is {peer.get('fetch_status')!r}")
    card = peer.get("card") or {}
    if card.get("tee") != "tdx" or not card.get("public_ip") or not card.get("ports"):
        fail(f"A2A peer {alias} card is incomplete: {card}")
    live = peer.get("live_status") or {}
    if live.get("state") != "ok":
        fail(f"A2A peer {alias} live_status is not ok: {live}")
PY
}
