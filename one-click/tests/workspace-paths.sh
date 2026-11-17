#!/usr/bin/env bash
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$ROOT_DIR/one-click/lib/common.sh"
. "$ROOT_DIR/one-click/lib/main.sh"
task_tmp="$(mktemp -d)"
trap 'rm -rf "$task_tmp"' EXIT
(
  cd "$task_tmp"
  unset CA_WORK_DIR CA_STATE_DIR
  init_defaults
  parse_args --state-dir 'custom state'
  normalize_workspace_paths
  apply_target_defaults
  [[ "$CA_STATE_DIR" == "$task_tmp/custom state" ]]
  [[ "$CA_WORK_DIR" == "$task_tmp/custom state/one-click" ]]
  [[ "$CA_SPEC_PATH" == "$CA_WORK_DIR/$CA_PROJECT_NAME/$CA_SPEC_NAME" ]]
)
(
  cd "$task_tmp"
  CA_WORK_DIR='env work'
  init_defaults
  parse_args --state-dir 'custom state' --work-dir 'chosen work' --model-key-file 'model.key' --cosign-key 'signing.key' --image-spec 'consumer.yaml'
  normalize_workspace_paths
  apply_target_defaults
  [[ "$CA_WORK_DIR" == "$task_tmp/chosen work" ]]
  [[ "$CA_MODEL_KEY_FILE" == "$task_tmp/model.key" ]]
  [[ "$CA_COSIGN_KEY" == "$task_tmp/signing.key" ]]
  [[ "$CA_IMAGE_SPEC" == "$task_tmp/consumer.yaml" ]]
  cd /
  [[ "$CA_STATE_DIR" == "$task_tmp/custom state" ]]
)
(
  cd "$task_tmp"
  CA_WORK_DIR='env work'
  init_defaults
  parse_args --state-dir 'custom state'
  normalize_workspace_paths
  [[ "$CA_WORK_DIR" == "$task_tmp/env work" ]]
)
printf 'workspace directory resolution passed\n'
