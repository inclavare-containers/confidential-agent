#!/usr/bin/env bash

matrix_expect_success() {
  local label="$1"
  local stdout="$WORK_DIR/$label.out"
  local stderr="$WORK_DIR/$label.err"
  shift
  log "matrix success: $label"
  ca_capture "$STATE_DIR" "$stdout" "$stderr" "$@"
  record_file_as_block "$label stdout:" "$stdout" text
  record_file_as_block "$label stderr:" "$stderr" text
}

matrix_expect_failure() {
  local label="$1"
  local expected="$2"
  local stdout="$WORK_DIR/$label.out"
  local stderr="$WORK_DIR/$label.err"
  shift 2
  log "matrix expected failure: $label"
  if ca_capture "$STATE_DIR" "$stdout" "$stderr" "$@"; then
    record_file_as_block "$label unexpected stdout:" "$stdout" text
    record_file_as_block "$label unexpected stderr:" "$stderr" text
    echo "command '$label' unexpectedly succeeded" >&2
    return 1
  fi
  record_file_as_block "$label stdout:" "$stdout" text
  record_file_as_block "$label stderr:" "$stderr" text
  if ! grep -Fqi -- "$expected" "$stderr" && ! grep -Fqi -- "$expected" "$stdout"; then
    echo "command '$label' failed without expected text '$expected'" >&2
    return 1
  fi
}

matrix_assert_json() {
  local path="$1"
  python3.11 -m json.tool "$path" >/dev/null
}

matrix_assert_contains() {
  local path="$1"
  local expected="$2"
  grep -Fq "$expected" "$path" || {
    echo "expected '$path' to contain '$expected'" >&2
    return 1
  }
}

matrix_assert_not_contains() {
  local path="$1"
  local expected="$2"
  if grep -Fq "$expected" "$path"; then
    echo "expected '$path' not to contain '$expected'" >&2
    return 1
  fi
}

matrix_generate_fixtures() {
  python3.11 - "$STATE_DIR" "$CONNECT_STATE_DIR" "$WORK_DIR" <<'PY'
import hashlib
import json
import sys
import time
from pathlib import Path

state_dir = Path(sys.argv[1])
connect_state_dir = Path(sys.argv[2])
work_dir = Path(sys.argv[3])
state_dir.mkdir(parents=True, exist_ok=True)
connect_state_dir.mkdir(parents=True, exist_ok=True)

valid_spec = work_dir / "matrix.yaml"
invalid_spec = work_dir / "invalid.yaml"
legacy_spec = work_dir / "legacy.yaml"
migrated_spec = work_dir / "migrated.yaml"
migrated_peerings = work_dir / "migrated-peerings.yaml"
work_dir.joinpath("install-matrix.sh").write_text("#!/usr/bin/env bash\nset -euo pipefail\necho matrix\n", encoding="utf-8")
work_dir.joinpath("resource.txt").write_text("matrix resource\n", encoding="utf-8")

valid_spec.write_text(
    f"""schema: confidential-agent/v1
service:
  id: matrix
  ports: [18080, 18789]
  connect: [18789]
  app_service: matrix.service
build:
  image_name: matrix-agent
  scripts: [{work_dir / "install-matrix.sh"}]
  variants:
    release:
      enabled: true
    debug:
      enabled: false
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 40
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources:
  matrix-resource:
    source: {work_dir / "resource.txt"}
    target: /etc/matrix-resource.txt
    mode: "0644"
""",
    encoding="utf-8",
)
invalid_spec.write_text(valid_spec.read_text(encoding="utf-8").replace("connect: [18789]", "connect: [19999]"), encoding="utf-8")
legacy_spec.write_text(
    f"""schema: confidential-agent/v1
service:
  id: legacy
  ports: [18789]
  connect: [18789]
build:
  image_name: legacy-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
    a2a_peer_cidrs:
      - 198.51.100.10/32
peers:
  - id: beta
    url: http://198.51.100.10:8089/.well-known/agent-card.json
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {{}}
""",
    encoding="utf-8",
)

def service_state(root: Path, service_id: str, phase: str, public_ip="", image_present=True):
    service_dir = root / "services" / service_id
    image_dir = service_dir / "shelter" / "images" / f"{service_id}-agent-release"
    image_dir.mkdir(parents=True, exist_ok=True)
    image_path = image_dir / "image.qcow2"
    if image_present:
        image_path.write_text("image", encoding="utf-8")
        if service_id == "matrix":
            linked_image = service_dir / "artifacts/current.qcow2"
            linked_image.parent.mkdir(parents=True, exist_ok=True)
            linked_image.unlink(missing_ok=True)
            linked_image.hardlink_to(image_path)
    source_sha = hashlib.sha256(image_path.read_bytes()).hexdigest() if image_path.exists() else "0" * 64
    build_id = f"{service_id}-agent-release"
    build_result = image_dir / "build-result.json"
    build_result.write_text(json.dumps({
        "id": build_id,
        "image_path": str(image_path),
        "reference_value": None,
        "rekor_value": None,
    }, indent=2), encoding="utf-8")
    manifest = {
        "service_id": service_id,
        "shelter_build_id": build_id,
        "shelter_work_dir": str(service_dir / "shelter"),
        "build_result": str(build_result),
        "deploy_result": str(service_dir / "shelter" / "terraform" / build_id / "deploy-result.json"),
        "shelter_config": str(service_dir / "shelter.yaml"),
        "agentd_bin": "/tmp/confidential-agentd",
        "agentd_service": str(service_dir / "guest" / "confidential-agentd.service"),
        "gateway_bin": "/tmp/cai-gateway",
        "gateway_service": str(service_dir / "guest" / "cai-gateway.service"),
        "tng_service": str(service_dir / "guest" / "trusted-network-gateway.service"),
        "fde_config_file": str(service_dir / "guest" / "cryptpilot.toml"),
        "policy_default": str(service_dir / "guest" / "default.rego"),
        "policy_local_dev": str(service_dir / "guest" / "local-dev.rego"),
        "images_dir": str(service_dir / "artifacts"),
        "cache_dir": str(service_dir / "cache"),
        "variants": {
            "release": {
                "shelter_build_id": build_id,
                "build_result": str(build_result),
            }
        },
    }
    (service_dir / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    published = {}
    if service_id == "matrix":
        published[f"aliyun/cn-beijing/release/{build_id}/{source_sha[:12]}"] = {
            "provider": "aliyun",
            "region": "cn-beijing",
            "variant": "release",
            "build_id": build_id,
            "source_sha256": source_sha,
            "source_size": image_path.stat().st_size,
            "status": "available",
            "image_name": f"ca-pub-{service_id}-release-{build_id}",
            "image_id": "m-matrixpublished",
            "import_task_id": "t-matrix",
            "bucket": "ca-images-cn-beijing-matrix",
            "object_key": f"confidential-agent/images/{service_id}/release/{build_id}/{source_sha[:16]}.qcow2",
            "created_at": "2026-06-02T00:00:00Z",
            "updated_at": "2026-06-02T00:00:00Z",
            "oss_cleaned": True,
        }
        published["aliyun/cn-beijing/release/old-build/source"] = {
            "provider": "aliyun",
            "region": "cn-beijing",
            "variant": "release",
            "build_id": "old-build",
            "source_sha256": "1" * 64,
            "source_size": 1,
            "status": "failed",
            "image_name": "ca-pub-matrix-old",
            "image_id": "m-oldmatrix",
            "bucket": "ca-images-cn-beijing-matrix",
            "object_key": "confidential-agent/images/matrix/release/old/image.qcow2",
            "created_at": "2026-06-02T00:00:00Z",
            "updated_at": "2026-06-02T00:00:00Z",
            "oss_cleaned": False,
            "error": "synthetic failed entry",
        }
    state = {
        "schema": "confidential-agent/service-state/v1",
        "service_id": service_id,
        "generation": 1,
        "phase": phase,
        "spec": {"path": str(valid_spec), "sha256": "spec"},
        "build": {
            "build_id": build_id,
            "image_name": f"{service_id}-agent",
            "variant": "release",
            "image_path": str(image_path),
            "images_dir": str(service_dir / "artifacts"),
            "cache_dir": str(service_dir / "cache"),
            "remote": False,
            "published": published,
        },
        "deploy": {
            "provider": "aliyun",
            "run_id": "run",
            "resource_name": f"{service_id}-run",
            "terraform_dir": str(service_dir / "terraform" / "active") if phase == "active" else None,
            "instance_id": "i-matrix" if phase == "active" else None,
            "security_group_id": "sg-matrix" if phase == "active" else None,
            "private_ip": "10.0.0.8" if phase == "active" else None,
            "public_ip": public_ip or None,
            "tee": "tdx",
        },
        "service": {"ports": [18080, 18789], "connect": [18789]},
        "resources": {},
        "mesh_generation": 1 if phase == "active" else 0,
        "reference_values": "sample",
    }
    (service_dir / "state.json").write_text(json.dumps(state, indent=2), encoding="utf-8")

service_state(state_dir, "matrix", "built")
service_state(state_dir, "active-rm", "deployed")
service_state(state_dir, "matrix-deleted", "deleted")
service_state(connect_state_dir, "matrix-active", "active", public_ip="203.0.113.10")
(connect_state_dir / "mesh-bundle.json").write_text(json.dumps({
    "schema": "confidential-agent/mesh-bundle/v1",
    "generation": 1,
    "updated_at": int(time.time()),
    "services": {
        "matrix-active": {
            "phase": "active",
            "private_ip": "10.0.0.10",
            "public_ip": "203.0.113.10",
            "ports": [18080, 18789],
            "connect": [18789],
        }
    },
    "reference_values": {
        "matrix-active": {"tdx": {"mr_td": "matrix-sample"}}
    },
    "rekor_reference_values": {},
}, indent=2), encoding="utf-8")

PY
}

matrix_run_local_commands() {
  matrix_expect_success version version

  matrix_expect_success docs-overview-json docs overview --format json
  matrix_assert_json "$WORK_DIR/docs-overview-json.out"
  matrix_expect_success docs-workflow docs workflow
  matrix_assert_contains "$WORK_DIR/docs-workflow.out" "# Workflow"

  matrix_expect_success spec-schema-json spec schema --format json
  matrix_assert_json "$WORK_DIR/spec-schema-json.out"
  matrix_expect_success spec-validate-json spec validate --spec "$WORK_DIR/matrix.yaml" --format json
  matrix_assert_json "$WORK_DIR/spec-validate-json.out"
  matrix_assert_contains "$WORK_DIR/spec-validate-json.out" '"ok": true'
  matrix_assert_contains "$WORK_DIR/spec-validate-json.out" '"service_id": "matrix"'
  matrix_expect_failure spec-validate-invalid "connect port" spec validate --spec "$WORK_DIR/invalid.yaml" --format json

  matrix_expect_success migrate-legacy migrate "$WORK_DIR/legacy.yaml" --out "$WORK_DIR/migrated.yaml" --peerings-out "$WORK_DIR/migrated-peerings.yaml"
  matrix_assert_contains "$WORK_DIR/migrated.yaml" "deploy:"
  matrix_assert_not_contains "$WORK_DIR/migrated.yaml" "security:"
  matrix_assert_not_contains "$WORK_DIR/migrated.yaml" "peers:"
  matrix_assert_contains "$WORK_DIR/migrated-peerings.yaml" "migrated-operator"
  matrix_assert_contains "$WORK_DIR/migrated-peerings.yaml" "migrated-peer-1"

  matrix_expect_success peering-list-empty peering list
  matrix_expect_success peering-add-ops peering add --role operator --cidr 203.0.113.0/24 --label ops
  matrix_expect_success peering-show-ops peering show ops
  matrix_assert_contains "$WORK_DIR/peering-show-ops.out" "label: ops"
  matrix_assert_contains "$WORK_DIR/peering-show-ops.out" "role: operator"
  matrix_expect_success peering-add-beta peering add --role peer --cidr 198.51.100.10/32 --label beta --scope agent-card,mesh
  matrix_expect_failure peering-duplicate "already exists" peering add --role peer --cidr 198.51.100.11/32 --label beta
  matrix_expect_failure peering-invalid-cidr "IPv4 CIDR" peering add --role peer --cidr not-a-cidr --label bad
  matrix_expect_success peering-apply-dry peering apply --dry-run
  matrix_expect_success peering-remove-beta peering remove beta
  matrix_assert_contains "$STATE_DIR/peerings.yaml" "label: ops"

  matrix_expect_success status-json status --json
  matrix_assert_json "$WORK_DIR/status-json.out"
  jq -e 'map(.service_id) | sort == ["active-rm", "matrix-deleted"]' "$WORK_DIR/status-json.out" >/dev/null
  matrix_expect_success status-service-json status --service active-rm --json
  matrix_assert_json "$WORK_DIR/status-service-json.out"
  jq -e 'length == 1 and .[0].service_id == "active-rm" and .[0].phase == "deployed"' "$WORK_DIR/status-service-json.out" >/dev/null
  matrix_expect_failure status-built "no local state for service 'matrix'" status --service matrix --json
  matrix_expect_success status-live-json status --live --json
  matrix_assert_json "$WORK_DIR/status-live-json.out"
  jq -e 'map(select(.local.service_id == "matrix-deleted")) | length == 1 and .[0].live_error == "service is not active or deployed"' "$WORK_DIR/status-live-json.out" >/dev/null
  jq -e 'map(select(.local.service_id == "active-rm")) | length == 1 and .[0].live_error == "service has no recorded instance address"' "$WORK_DIR/status-live-json.out" >/dev/null

  matrix_expect_success report-json report --json
  matrix_assert_json "$WORK_DIR/report-json.out"
  matrix_assert_contains "$WORK_DIR/report-json.out" '"collect_status": "skipped"'
  matrix_expect_success report-out report --out "$WORK_DIR/report.json"
  matrix_assert_json "$WORK_DIR/report.json"

  matrix_expect_success image-list-json image list --json
  matrix_assert_json "$WORK_DIR/image-list-json.out"
  matrix_assert_contains "$WORK_DIR/image-list-json.out" "m-matrixpublished"
  matrix_expect_success image-prune-dry image prune --dry-run
  matrix_expect_failure image-rm-active "destroy it before removing" image rm active-rm --force
  cp "$STATE_DIR/services/matrix/state.json" "$WORK_DIR/image-rm-state-before.json"
  cp "$STATE_DIR/services/matrix/manifest.json" "$WORK_DIR/image-rm-manifest-before.json"
  matrix_expect_success image-rm-built image rm matrix
  python3.11 - "$STATE_DIR/services/matrix" "$WORK_DIR" <<'PY'
import json
from pathlib import Path
import sys

service, work = map(Path, sys.argv[1:])
state = json.loads((service / "state.json").read_text())
assert not Path(state["build"]["image_path"]).exists(), "image rm retained its local payload"
assert not (service / "artifacts/current.qcow2").exists(), "image rm retained a payload hardlink"
assert (service / "state.json").read_bytes() == (work / "image-rm-state-before.json").read_bytes(), "image rm changed publication or management records"
assert (service / "manifest.json").read_bytes() == (work / "image-rm-manifest-before.json").read_bytes(), "image rm changed build metadata"
PY
  matrix_expect_success image-list-after-rm image list --json
  jq -e 'map(select(.service_id == "matrix")) | length > 0 and all(.image_present == false) and any(.[] | .publications[]; .image_id == "m-matrixpublished")' "$WORK_DIR/image-list-after-rm.out" >/dev/null

  matrix_generate_fixtures
  matrix_expect_failure image-publish-mismatch "does not match requested service" image publish wrong --spec "$WORK_DIR/matrix.yaml" --no-wait
  matrix_expect_success image-unpublish-no-match image unpublish matrix --image-id m-not-present

  matrix_expect_success a2a-list-empty a2a list
  matrix_expect_success a2a-add-beta a2a add --alias beta --service matrix http://127.0.0.1:1/.well-known/agent-card.json
  matrix_expect_success a2a-show-beta a2a show beta
  matrix_assert_json "$WORK_DIR/a2a-show-beta.out"
  matrix_expect_success a2a-sync-beta a2a sync --alias beta
  matrix_expect_failure a2a-sync-conflict "use either --all or --alias" a2a sync --all --alias beta
  matrix_expect_failure a2a-alias-conflict "conflicts with a local service id" a2a add --alias matrix http://127.0.0.1:2/.well-known/agent-card.json
  matrix_expect_failure a2a-signer-pair "--signer-issuer and --signer-subject" a2a add --signer-issuer https://issuer.example http://127.0.0.1:3/.well-known/agent-card.json
  matrix_expect_success a2a-remove-beta a2a remove beta
  matrix_assert_json "$STATE_DIR/a2a-bundle.json"

  local old_state_dir="$STATE_DIR"
  STATE_DIR="$CONNECT_STATE_DIR"
  matrix_expect_success connect-render connect --render-only
  matrix_assert_json "$WORK_DIR/connect-render.out"
  matrix_assert_contains "$WORK_DIR/connect-render.out" '"client_endpoints"'
  matrix_assert_contains "$WORK_DIR/connect-render.out" "203.0.113.10"
  matrix_expect_success connect-render-service connect --service matrix-active --render-only
  matrix_assert_json "$WORK_DIR/connect-render-service.out"
  matrix_expect_failure connect-missing "no local state for service" connect --service missing --render-only
  matrix_expect_failure connect-render-start-conflict "does not support --render-only" connect --render-only start --service matrix-active
  matrix_expect_failure connect-from-card-service-conflict "accepts either --from-card or --service" connect start --from-card http://127.0.0.1:1/.well-known/agent-card.json --service matrix-active
  STATE_DIR="$old_state_dir"

  matrix_expect_failure key-existing "already exists" key generate-cosign --output-key-prefix "$WORK_DIR/existing-cosign"
  matrix_expect_failure ssh-no-debug "no debug SSH key" ssh active-rm -- -V
}

matrix_published_summary() {
  local state_dir="$1"
  local service="$2"
  local out="$3"
  python3.11 - "$state_dir/services/$service/state.json" >"$out" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)
published = state.get("build", {}).get("published", {})
for key, entry in published.items():
    print(f"{key} {entry.get('status')} {entry.get('image_id', '-') } {entry.get('bucket', '-') } {entry.get('object_key', '-') }")
PY
}

matrix_first_published_image_id() {
  local state_dir="$1"
  local service="$2"
  python3.11 - "$state_dir/services/$service/state.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)
for entry in state.get("build", {}).get("published", {}).values():
    image_id = entry.get("image_id")
    if image_id:
        print(image_id)
        break
PY
}

matrix_aliyun_json() {
  local region="$1"
  shift
  local access_key="${ALICLOUD_ACCESS_KEY:-${ALIBABA_CLOUD_ACCESS_KEY_ID:-}}"
  local secret_key="${ALICLOUD_SECRET_KEY:-${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}}"
  local sts_token="${ALICLOUD_STS_TOKEN:-${ALIBABA_CLOUD_SECURITY_TOKEN:-}}"
  ALICLOUD_ACCESS_KEY="$access_key" ALICLOUD_SECRET_KEY="$secret_key" \
  ALIBABA_CLOUD_ACCESS_KEY_ID="$access_key" ALIBABA_CLOUD_ACCESS_KEY_SECRET="$secret_key" \
  ALICLOUD_STS_TOKEN="$sts_token" ALIBABA_CLOUD_SECURITY_TOKEN="$sts_token" \
    "$TOOLS_DIR/aliyun" --region "$region" "$@"

}

matrix_wait_cloud_image_available() {
  local image_id="$1"
  local region="$2"
  local log_path="$WORK_DIR/publish-image-status.log"
  local image_deadline
  image_deadline=$((SECONDS + ${E2E_MATRIX_IMAGE_TIMEOUT_SEC:-1800}))
  : >"$log_path"
  while (( SECONDS < image_deadline )); do
    local status
    status="$(
      matrix_aliyun_json "$region" \
        ecs DescribeImages \
        --Status ALL \
        --RegionId "$region" \
        --ImageId "$image_id" \
        --ImageOwnerAlias self |
        python3.11 -c 'import json,sys; data=json.load(sys.stdin); imgs=data.get("Images",{}).get("Image",[]); print((imgs[0] or {}).get("Status","") if imgs else "")'
    )"
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${status:-missing}" | tee -a "$log_path"
    case "$status" in
      Available) break ;;
      CreateFailed) echo "published image $image_id failed to import" >&2; return 1 ;;
      *) sleep 30 ;;
    esac
  done
  if (( SECONDS >= image_deadline )); then
    record_file_as_block "Published image status log at timeout:" "$log_path" text
    echo "published image $image_id did not become available before timeout" >&2
    return 1
  fi
  record_file_as_block "Published image status log:" "$log_path" text
}

matrix_verify_cloud_image_deleted() {
  local image_id="$1"
  local region="$2"
  local out="$WORK_DIR/deleted-image-check.json"
  matrix_aliyun_json "$region" ecs DescribeImages --Status ALL --RegionId "$region" --ImageId "$image_id" --ImageOwnerAlias self >"$out"
  record_file_as_block "Deleted image verification:" "$out" json
  python3.11 - "$out" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
if data.get("Images", {}).get("Image", []):
    raise SystemExit("custom image still exists after unpublish")
PY
}

matrix_generate_real_cloud_fixture() {
  local fixture_dir="$MATRIX_REAL_CASE_DIR/matrix"
  local install_script="$fixture_dir/install-matrix-cloud.sh"
  MATRIX_REAL_SPEC="$fixture_dir/matrix.yaml"

  local cosign_key=""
  if [[ "$MATRIX_REAL_REFERENCE_VALUES" == "rekor" ]]; then
    [[ -x "$SLSA_GENERATOR" ]] || {
      echo "SLSA generator is required for matrix real-cloud rekor mode: $SLSA_GENERATOR" >&2
      return 1
    }
    cosign_key="$(resolve_cosign_key)"
  fi

  record_cmd "python3.11 tools/e2e/render_case.py --case cli-command-matrix --work-dir $(printf '%q' "$MATRIX_REAL_WORK_DIR")"
  REFERENCE_VALUES="$MATRIX_REAL_REFERENCE_VALUES" \
    COSIGN_KEY="$cosign_key" \
    REGION="$MATRIX_REAL_REGION" \
    ZONE_ID="$MATRIX_REAL_ZONE_ID" \
    INSTANCE_TYPE="$MATRIX_REAL_INSTANCE_TYPE" \
    DISK_GB="${E2E_MATRIX_DISK_GB:-80}" \
    SLSA_GENERATOR="$SLSA_GENERATOR" \
    MATRIX_REAL_SERVICE_ID="$MATRIX_REAL_SERVICE_ID" \
    MATRIX_REAL_REFERENCE_VALUES="$MATRIX_REAL_REFERENCE_VALUES" \
    MATRIX_REAL_COSIGN_KEY="$cosign_key" \
    MATRIX_REAL_SLSA_GENERATOR="$SLSA_GENERATOR" \
    MATRIX_REAL_REGION="$MATRIX_REAL_REGION" \
    MATRIX_REAL_ZONE_ID="$MATRIX_REAL_ZONE_ID" \
    MATRIX_REAL_INSTANCE_TYPE="$MATRIX_REAL_INSTANCE_TYPE" \
    MATRIX_REAL_DISK_GB="${E2E_MATRIX_DISK_GB:-80}" \
    MATRIX_REAL_INSTALL_SCRIPT="$install_script" \
    python3.11 "$ROOT_DIR/tools/e2e/render_case.py" \
      --case cli-command-matrix \
      --work-dir "$MATRIX_REAL_WORK_DIR"
  chmod 0755 "$install_script"
}

matrix_run_real_cloud_publish_flow() {
  MATRIX_REAL_REGION="${E2E_REGION:-cn-hongkong}"
  MATRIX_REAL_ZONE_ID="${E2E_ZONE_ID:-$(default_tdx_zone_id "$MATRIX_REAL_REGION")}"
  MATRIX_REAL_INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-$(default_tdx_instance_type "$MATRIX_REAL_REGION")}"
  MATRIX_REAL_WORK_DIR="$WORK_DIR/real-cloud"
  MATRIX_REAL_STATE_DIR="$MATRIX_REAL_WORK_DIR/state"
  MATRIX_REAL_CASE_DIR="$MATRIX_REAL_WORK_DIR/rendered"
  MATRIX_REAL_SERVICE_ID="${E2E_MATRIX_SERVICE_ID:-matrix-${E2E_RUN_ID//[^a-zA-Z0-9-]/-}}"
  MATRIX_REAL_REFERENCE_VALUES="${E2E_MATRIX_REFERENCE_VALUES:-sample}"
  mkdir -p "$MATRIX_REAL_WORK_DIR" "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_CASE_DIR"

  require_cmd aliyun
  require_cmd curl
  require_cmd ssh
  require_aliyun_credentials
  ensure_shelter

  log "generating real-cloud matrix fixture"
  matrix_generate_real_cloud_fixture

  validate_specs "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SPEC"
  register_destroy_target "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID"

  ca_run "$MATRIX_REAL_STATE_DIR" build --spec "$MATRIX_REAL_SPEC"
  record_manifest_variants "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID"

  ca_run "$MATRIX_REAL_STATE_DIR" image publish "$MATRIX_REAL_SERVICE_ID" --spec "$MATRIX_REAL_SPEC" --region "$MATRIX_REAL_REGION" --no-wait
  matrix_published_summary "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID" "$WORK_DIR/published-after-no-wait.txt"
  record_file_as_block "Published state after no-wait:" "$WORK_DIR/published-after-no-wait.txt" text
  matrix_assert_contains "$WORK_DIR/published-after-no-wait.txt" "importing"
  local image_id
  image_id="$(matrix_first_published_image_id "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID")"
  test -n "$image_id" || {
    echo "image publish --no-wait did not record an image id" >&2
    return 1
  }

  matrix_wait_cloud_image_available "$image_id" "$MATRIX_REAL_REGION"
  ca_run "$MATRIX_REAL_STATE_DIR" image publish "$MATRIX_REAL_SERVICE_ID" --spec "$MATRIX_REAL_SPEC" --region "$MATRIX_REAL_REGION"
  matrix_published_summary "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID" "$WORK_DIR/published-after-wait.txt"
  record_file_as_block "Published state after wait:" "$WORK_DIR/published-after-wait.txt" text
  matrix_assert_contains "$WORK_DIR/published-after-wait.txt" "available"

  ensure_operator_peering "$MATRIX_REAL_STATE_DIR" ops "$(resolve_allowed_cidr)"
  ca_capture "$MATRIX_REAL_STATE_DIR" "$WORK_DIR/deploy-render.out" "$WORK_DIR/deploy-render.err" deploy --spec "$MATRIX_REAL_SPEC" --render-only --skip-peering-check
  local rendered_config
  rendered_config="$(tail -n 1 "$WORK_DIR/deploy-render.out")"
  record_file_as_block "Deploy render stdout:" "$WORK_DIR/deploy-render.out" text
  record_file_as_block "Deploy rendered Shelter config:" "$rendered_config" yaml
  matrix_assert_contains "$rendered_config" "image_id: $image_id"
  matrix_assert_not_contains "$rendered_config" "nvme_support:"

  E2E_DEPLOY_ATTEMPTED=1
  ca_run "$MATRIX_REAL_STATE_DIR" deploy --spec "$MATRIX_REAL_SPEC"

  local public_ip
  public_ip="$(state_value "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID" deploy.public_ip)"
  test -n "$public_ip" || {
    echo "deploy did not record public_ip" >&2
    return 1
  }
  local status_deadline
  status_deadline=$((SECONDS + ${E2E_MATRIX_STATUS_TIMEOUT_SEC:-1200}))
  while (( SECONDS < status_deadline )); do
    if ca_capture "$MATRIX_REAL_STATE_DIR" "$WORK_DIR/status-live-real.json" "$WORK_DIR/status-live-real.err" status --live --json; then
      if python3.11 - "$WORK_DIR/status-live-real.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    items = json.load(f)
item = items[0] if isinstance(items, list) else items
daemon = item.get("daemon") or {}
raise SystemExit(0 if daemon.get("app_ready") is True and daemon.get("mesh_ready") is True else 1)
PY
      then
        break
      fi
    fi
    sleep 30
  done
  if (( SECONDS >= status_deadline )); then
    record_file_as_block "Real live status stdout at timeout:" "$WORK_DIR/status-live-real.json" json
    record_file_as_block "Real live status stderr at timeout:" "$WORK_DIR/status-live-real.err" text
    echo "timed out waiting for matrix real-cloud live status readiness" >&2
    return 1
  fi
  record_file_as_block "Real live status:" "$WORK_DIR/status-live-real.json" json
  ca_capture "$MATRIX_REAL_STATE_DIR" "$WORK_DIR/report-real.json" "$WORK_DIR/report-real.err" report --service "$MATRIX_REAL_SERVICE_ID" --json
  matrix_assert_json "$WORK_DIR/report-real.json"
  record_file_as_block "Real report:" "$WORK_DIR/report-real.json" json

  ca_run "$MATRIX_REAL_STATE_DIR" destroy "$MATRIX_REAL_SERVICE_ID"
  ca_run "$MATRIX_REAL_STATE_DIR" image prune --dry-run --all
  ca_run "$MATRIX_REAL_STATE_DIR" image unpublish "$MATRIX_REAL_SERVICE_ID" --image-id "$image_id" --force
  matrix_verify_cloud_image_deleted "$image_id" "$MATRIX_REAL_REGION"
}

case_cleanup() {
  local status="$1"
  if [[ "$status" == "0" || "${E2E_MATRIX_REAL_CLOUD:-0}" != "1" ]]; then
    return 0
  fi
  if [[ -n "${MATRIX_REAL_STATE_DIR:-}" && -n "${MATRIX_REAL_SERVICE_ID:-}" && -f "$MATRIX_REAL_STATE_DIR/services/$MATRIX_REAL_SERVICE_ID/state.json" ]]; then
    local image_id=""
    image_id="$(matrix_first_published_image_id "$MATRIX_REAL_STATE_DIR" "$MATRIX_REAL_SERVICE_ID" 2>/dev/null || true)"
    if [[ "$E2E_DEPLOY_ATTEMPTED" == "1" ]]; then
      log "destroying $MATRIX_REAL_SERVICE_ID before failed real-cloud lane image cleanup"
      ca_run "$MATRIX_REAL_STATE_DIR" destroy "$MATRIX_REAL_SERVICE_ID" || return
      E2E_DEPLOY_ATTEMPTED=0
    fi
    if [[ -n "$image_id" && -n "${MATRIX_REAL_SPEC:-}" && -f "$MATRIX_REAL_SPEC" ]]; then
      log "waiting for published image $image_id to settle before failed-lane cleanup"
      ca_run "$MATRIX_REAL_STATE_DIR" image publish "$MATRIX_REAL_SERVICE_ID" --spec "$MATRIX_REAL_SPEC" --region "$MATRIX_REAL_REGION" || true
    fi
    log "unpublishing $MATRIX_REAL_SERVICE_ID after failed real-cloud lane"
    ca_run "$MATRIX_REAL_STATE_DIR" image unpublish "$MATRIX_REAL_SERVICE_ID" --force
  fi
}

case_post_destroy_cleanup() {
  local status="$1"
  [[ "${E2E_MATRIX_REAL_CLOUD:-0}" == "1" ]] || return 0
  if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" != "1" ]] ||
     [[ "$status" != "0" && "$DESTROY_ON_FAILURE" != "1" ]]; then
    return 0
  fi
  local buckets=() bucket cleanup_status=0
  mapfile -t buckets < <(python3.11 - "$WORK_DIR" <<'PYBUCKET'
import re
import sys
from pathlib import Path
buckets = set()
for path in Path(sys.argv[1]).glob("published-after-*.txt"):
    for line in path.read_text().splitlines():
        fields = line.split()
        if len(fields) >= 4 and re.fullmatch(r"ca-images-[a-z0-9-]+", fields[3]):
            buckets.add(fields[3])
print("\n".join(sorted(buckets)))
PYBUCKET
  )
  for bucket in "${buckets[@]}"; do
    [[ -n "$bucket" ]] || continue
    # CA retains publish buckets for reuse; this test owns a fresh state and
    # removes only its empty bucket. Never recursively delete bucket contents.
    ALIBABA_CLOUD_REGION_ID="$MATRIX_REAL_REGION" aliyun oss rm "oss://$bucket" -b -f \
      -e "oss-$MATRIX_REAL_REGION.aliyuncs.com" || cleanup_status=1
  done
  return "$cleanup_status"
}

run_case() {
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/cli-command-matrix-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  CONNECT_STATE_DIR="$WORK_DIR/connect-state"
  CONNECT_STATE_DIR="$(absolute_dir "$CONNECT_STATE_DIR")"

  require_cmd cargo
  require_cmd jq
  require_cmd python3.11

  init_step_log "Confidential Agent CLI Command Matrix E2E"
  install_exit_traps
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway
  matrix_generate_fixtures
  touch "$WORK_DIR/existing-cosign.key" "$WORK_DIR/existing-cosign.pub"

  matrix_run_local_commands

  if [[ "${E2E_MATRIX_REAL_CLOUD:-0}" == "1" ]]; then
    matrix_run_real_cloud_publish_flow
  else
    record "- real cloud publish/deploy lane skipped; set E2E_MATRIX_REAL_CLOUD=1 to enable."
  fi
}
