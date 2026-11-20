#!/usr/bin/env bash

# The checkpoint is data read by Bash before host dependencies are installed.
# List fields explicitly so that credentials never become part of the task.
cpu_task_fields=(
  CA_CPU_NEXT CA_CPU_DONE CA_CPU_ROOT CA_CPU_GATEWAY_TOKEN_REQUIRED
  CA_SERVICE_ID CA_WORK_DIR CA_WORK_DIR_EXPLICIT CA_TOOLS_DIR
  CA_PACKAGED_INSTALL CA_RELEASE_VERSION CA_NON_INTERACTIVE CA_ASSUME_YES
  CA_ONE_CLICK_ENTRY_URL CA_BOOTSTRAP_ACTION CA_BOOTSTRAP_ROOT CA_BOOTSTRAP_REPO CA_BOOTSTRAP_REF
  CA_SKIP_DEPS CA_SKIP_RUSTUP CA_SKIP_CARGO_BUILD CA_SKIP_BUILD CA_SKIP_DEPLOY
  CA_SKIP_HOST_CLIENT CA_SKIP_HOST_OPENCLAW CA_DISABLE_PEP CA_ENABLE_DINGTALK
  CA_START_CONNECT CA_REGION CA_ZONE_ID CA_INSTANCE_TYPE CA_DISK_GB CA_REPLICAS
  CA_REFERENCE_VALUES CA_HARNESS CA_HARNESS_MODE CA_ACCESS_METHOD CA_MODEL CA_API_BASE_URL
  CA_IMAGE_SOURCE CA_IMAGE_ID CA_ARTIFACT CA_IMAGE_RELEASE CA_RELEASE_DIGEST CA_DOWNLOAD_DIR
  CA_OFFICIAL_METADATA_DIR CA_RELEASE_IMAGE_CACHE CA_COSIGN_KEY CA_SLSA_GENERATOR
  CA_OPENCLAW_VERSION CA_NODE_VERSION CA_NPM_REGISTRY
  CA_ALLOWED_CIDRS CA_ALLOWED_CIDR CA_DEPLOYER_CIDR CA_OPERATOR_EGRESS_IP
  CA_APPROVED_PEERING_REPLACEMENTS CA_CONNECT_READINESS_PATH CA_READY_JSON
  CA_CONNECT_HOST CA_CONNECT_PORT CA_CONNECT_PID
  CA_STATUS_TIMEOUT_SEC CA_CONNECT_TIMEOUT_SEC CA_SHELTER_BIN CA_SHELTER_RPM
  CA_SHELTER_RPM_CACHE CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN CA_INSTALL_CLI
)

cpu_task_path() {
  printf '%s/one-click-tasks/%s.state\n' "$CA_STATE_DIR" "$1"
}

save_cpu_task() {
  local path tmp field
  path="$(cpu_task_path "$CA_SERVICE_ID")"
  install -d -m 0700 "${path%/*}"
  tmp="$(mktemp "${path}.XXXXXX")"
  for field in "${cpu_task_fields[@]}" CA_HOST_CLIENT; do
    printf '%s\0' "${!field-}"
  done >"$tmp"
  mv -f -- "$tmp" "$path"
}

load_cpu_task() {
  local path field
  path="$(cpu_task_path "$1")"
  [[ -f "$path" ]] || die "no unfinished CPU task for service '$1'"
  {
    for field in "${cpu_task_fields[@]}"; do
      IFS= read -r -d '' "$field" || die "could not read CPU task in $path"
    done
    # Older tasks did not record consent to install an application client.
    IFS= read -r -d '' CA_HOST_CLIENT || CA_HOST_CLIENT=""
  } <"$path"
  CA_SERVICE_ID_OVERRIDE="$CA_SERVICE_ID"
  if [[ -n "$CA_CPU_ROOT" ]]; then ROOT_DIR="$CA_CPU_ROOT"; fi
  if [[ -n "$CA_SHELTER_BIN" ]]; then export CA_SHELTER_BIN; fi
  if [[ -n "$CA_OPERATOR_EGRESS_IP" ]]; then export CA_OPERATOR_EGRESS_IP; fi
}

resume_cpu_task() {
  local id="${CA_SERVICE_ID_OVERRIDE:-}" path non_interactive="$CA_NON_INTERACTIVE"
  local -a ids=()
  if [[ -z "$id" ]]; then
    for path in "$CA_STATE_DIR"/one-click-tasks/*.state; do
      [[ -f "$path" ]] || continue
      path="${path##*/}"
      ids+=("${path%.state}")
    done
    case "${#ids[@]}" in
      0) die "no unfinished CPU one-click tasks in $CA_STATE_DIR" ;;
      1) id="${ids[0]}" ;;
      *)
        printf 'Unfinished CPU one-click tasks:\n' >&2
        printf '  %s\n' "${ids[@]}" >&2
        die 'run continue --service-id ID to select a task'
        ;;
    esac
  fi
  [[ "$id" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid service ID: $id"
  load_cpu_task "$id"
  if [[ "$non_interactive" == 1 ]]; then CA_NON_INTERACTIVE=1; fi
  CA_MODE=deploy-openclaw
  CA_CPU_RESUMING=1
  log "continuing service '$id' from $CA_CPU_NEXT"
}

reject_pending_cpu_task() {
  local path entry
  path="$(cpu_task_path "$CA_SERVICE_ID")"
  if [[ -f "$path" ]]; then
    load_cpu_task "$CA_SERVICE_ID"
    log "service '$CA_SERVICE_ID' has an unfinished task at $CA_CPU_NEXT"
    entry="$ROOT_DIR/one-click/install.sh"
    if [[ -n "$CA_ONE_CLICK_ENTRY_URL" ]] && ! cpu_step_done bootstrap; then
      printf '  curl -fsSL %q | sh -s --' "$CA_ONE_CLICK_ENTRY_URL"
      if [[ "$CA_BOOTSTRAP_ACTION" == source ]]; then
        printf ' --repo %q --ref %q --source-dir %q' "$CA_BOOTSTRAP_REPO" "$CA_BOOTSTRAP_REF" "$CA_BOOTSTRAP_ROOT"
      fi
    else
      printf '  %q' "$entry"
    fi
    printf ' continue --state-dir %q --service-id %q\n' "$CA_STATE_DIR" "$CA_SERVICE_ID"
    exit 1
  fi
}

cpu_step_done() {
  [[ " ${CA_CPU_DONE:-} " == *" $1 "* ]]
}

cpu_step() {
  local step="$1"
  shift
  if cpu_step_done "$step"; then return; fi
  CA_CPU_NEXT="$step"
  save_cpu_task
  "$@"
  CA_CPU_DONE="${CA_CPU_DONE:-} $step"
  save_cpu_task
}

run_cleanup_cpu() {
  local path
  path="$(cpu_task_path "$CA_SERVICE_ID")"
  if [[ -f "$path" ]]; then
    load_cpu_task "$CA_SERVICE_ID"
    normalize_workspace_paths
    apply_target_defaults
    if [[ "$CA_CPU_NEXT" != deploy ]] && ! cpu_step_done deploy; then
      rm -- "$path"
      log "cleared unfinished task for '$CA_SERVICE_ID'"
      return
    fi
  fi
  collect_aliyun_credentials
  run_cleanup_service
  if [[ -f "$path" ]]; then rm -- "$path"; fi
}
