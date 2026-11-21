#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT_DIR/one-click/lib/main.sh"

if [[ "${CPU_CONTINUE_FIXTURE:-}" ]]; then
  note() { printf '%s\n' "$*" >>"$CPU_CONTINUE_FIXTURE/steps"; }
  collect_aliyun_credentials() { note credentials; }
  configure_aliyun_credentials() { :; }
  ensure_bailian_credentials() { export DASHSCOPE_API_KEY=private-model-key; }
  resolve_allowed_cidr() { CA_ALLOWED_CIDRS=192.0.2.1/32; }
  prepare_operator_peering() { :; }
  prepare_one_click_source() { note bootstrap; }
  install_os_dependencies() { note dependencies; }
  ensure_host_access_client() { :; }
  ensure_shelter() { :; }
  build_confidential_agent() { :; }
  install_confidential_agent_cli() { :; }
  ensure_native_tools() { :; }
  resolve_cpu_release_metadata() {
    CA_IMAGE_RELEASE="$CPU_CONTINUE_FIXTURE/release.json"
    CA_RELEASE_DIGEST=sha256:release
    CA_DOWNLOAD_DIR="$CA_WORK_DIR/image"
    note metadata
  }
  find_cpu_publication() {
    note lookup
    if [[ -f "$CPU_CONTINUE_FIXTURE/reuse" ]]; then CA_IMAGE_ID=m-existing; fi
  }
  download_cpu_image() {
    note download
    local partial="$CPU_CONTINUE_FIXTURE/$CA_SERVICE_ID.part"
    if [[ ! -f "$partial" ]]; then
      : >"$partial"
      return 23
    fi
    CA_ARTIFACT="$CA_DOWNLOAD_DIR/artifact"
  }
  publish_cpu_image() { CA_IMAGE_ID=m-existing; note publish; }
  prepare_cpu_specs() {
    mkdir -p "$CA_PROJECT_DIR"
    printf '%s\n' "$CA_MODEL" >"$CA_SPEC_PATH"
    note spec
  }
  ensure_operator_peering() { note peering; }
  ca_cmd() {
    case "$*" in
      'status --json')
        local phase=none
        if [[ -f "$CPU_CONTINUE_FIXTURE/phase" ]]; then
          read -r phase <"$CPU_CONTINUE_FIXTURE/phase"
        fi
        printf '[{"service_id":"%s","phase":"%s"}]\n' "$CA_SERVICE_ID" "$phase"
        ;;
      'deploy --spec '*)
        note 'deploy initially'
        printf 'deploying\n' >"$CPU_CONTINUE_FIXTURE/phase"
        return 27
        ;;
      'deploy --resume --spec '*)
        note 'deploy with resume'
        printf 'active\n' >"$CPU_CONTINUE_FIXTURE/phase"
        ;;
      *) return 1 ;;
    esac
  }
  wait_for_live_status() { note readiness; }
  start_connect() {
    note connect
    if [[ ! -f "$CPU_CONTINUE_FIXTURE/failed-connect" ]]; then
      : >"$CPU_CONTINUE_FIXTURE/failed-connect"
      return 33
    fi
  }
  print_cpu_summary() { note summary; }
  run_cleanup_service() { note cleanup; }
  main "$@" --state-dir "$CPU_CONTINUE_FIXTURE/state" --non-interactive
  exit
fi

export CPU_CONTINUE_FIXTURE
CPU_CONTINUE_FIXTURE="$(mktemp -d)"
trap 'rm -rf -- "$CPU_CONTINUE_FIXTURE"' EXIT

run() {
  local expected="$1" actual=0
  shift
  bash "$0" "$@" >"$CPU_CONTINUE_FIXTURE/output" 2>&1 || actual=$?
  if [[ "$actual" != "$expected" ]]; then
    cat "$CPU_CONTINUE_FIXTURE/output" >&2
    printf 'expected exit %s, got %s for %s\n' "$expected" "$actual" "$*" >&2
    exit 1
  fi
}

run 23 deploy --harness codex --service-id work --model original-model
checkpoint="$CPU_CONTINUE_FIXTURE/state/one-click-tasks/work.state"
[[ -f "$checkpoint" ]]
if grep -aq 'private-model-key' "$checkpoint"; then exit 1; fi
run 1 deploy --harness codex --service-id work
grep -Fq 'unfinished task at download' "$CPU_CONTINUE_FIXTURE/output"
grep -Fq -- '--service-id work' "$CPU_CONTINUE_FIXTURE/output"

CA_MODEL=unrelated-model run 27 continue
grep -Fxq original-model "$CPU_CONTINUE_FIXTURE/state/one-click/work/instance.yaml"
run 1 continue --service-id work
grep -Fq 'the cloud outcome is unknown' "$CPU_CONTINUE_FIXTURE/output"
printf 'deployed\n' >"$CPU_CONTINUE_FIXTURE/phase"
run 33 continue --service-id work
run 0 continue --service-id work
[[ ! -f "$checkpoint" ]]

count_step() { [[ "$(grep -Fxc "$1" "$CPU_CONTINUE_FIXTURE/steps")" == "$2" ]]; }
count_step bootstrap 1
count_step dependencies 1
count_step metadata 1
count_step lookup 1
count_step download 2
count_step publish 1
count_step spec 1
count_step peering 1
count_step 'deploy initially' 1
count_step 'deploy with resume' 1
count_step readiness 1
count_step connect 2
count_step summary 1

run 23 deploy --harness codex --service-id first
run 23 deploy --harness codex --service-id second
run 1 continue
grep -Fxq '  first' "$CPU_CONTINUE_FIXTURE/output"
grep -Fxq '  second' "$CPU_CONTINUE_FIXTURE/output"
run 0 cleanup --service-id first
if grep -Fxq cleanup "$CPU_CONTINUE_FIXTURE/steps"; then exit 1; fi
[[ -f "$CPU_CONTINUE_FIXTURE/state/one-click-tasks/second.state" ]]
[[ ! -f "$CPU_CONTINUE_FIXTURE/state/one-click-tasks/first.state" ]]

printf 'none\n' >"$CPU_CONTINUE_FIXTURE/phase"
run 27 continue --service-id second
run 0 cleanup --service-id second
count_step cleanup 1
[[ ! -f "$CPU_CONTINUE_FIXTURE/state/one-click-tasks/second.state" ]]

: >"$CPU_CONTINUE_FIXTURE/reuse"
printf 'none\n' >"$CPU_CONTINUE_FIXTURE/phase"
run 27 deploy --harness codex --service-id reuse --verify-disk
grep -Fq 'using ECS image m-existing; --verify-disk checks local artifacts only, and no local disk was read' "$CPU_CONTINUE_FIXTURE/output"
count_step download 5
count_step publish 2
printf 'CPU one-click checkpoints, task selection, guarded deploy resume, access, and cleanup passed\n'
