#!/usr/bin/env bash
# Run after cargo build -p confidential-agent-cli -p cai-pep.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT_DIR/one-click/lib/main.sh"
task_tmp="$(mktemp -d)"
trap 'rm -rf "$task_tmp"' EXIT
export CA_BIN="${CA_TEST_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
export CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"
export CA_NON_INTERACTIVE=1
unset CA_HARNESS CA_MODEL CA_BAILIAN_MODEL DASHSCOPE_MODEL CA_API_BASE_URL DASHSCOPE_BASE_URL
unset CA_API_KEY CA_BAILIAN_API_KEY DASHSCOPE_API_KEY BAILIAN_API_KEY
printf '{}\n' > "$task_tmp/release.json"
printf 'publisher fixture\n' > "$task_tmp/publisher.pem"
printf 'cosign fixture\n' > "$task_tmp/cosign.key"
export CA_SLSA_GENERATOR=/bin/true

(
  init_defaults
  resolve_harness_options
  apply_target_defaults
  [[ "$CA_HARNESS" == openclaw && "$CA_MODEL" == qwen4-max ]]
  [[ "$CA_SERVICE_ID" == openclaw && "$CA_SPEC_NAME" == openclaw.yaml && "$CA_CONNECT_NAME" == connect ]]
  [[ "$CA_API_BASE_URL" == https://dashscope.aliyuncs.com/compatible-mode/v1 ]]
  parse_args --bailian-api-key legacy-key --bailian-model legacy-model
  ensure_bailian_credentials
  [[ "$DASHSCOPE_API_KEY" == legacy-key && "$CA_MODEL" == legacy-model ]]
)
(
  CA_NON_INTERACTIVE=0
  init_defaults
  resolve_harness_options <<< $'claude-code\nchosen-model\nhttps://inference.example/anthropic'
  [[ "$CA_HARNESS" == claude-code && "$CA_MODEL" == chosen-model ]]
  [[ "$CA_API_BASE_URL" == https://inference.example/anthropic ]]
)
(
  CA_MODEL=env-model CA_API_KEY=env-key CA_HARNESS=claude-code
  init_defaults
  resolve_harness_options
  ensure_bailian_credentials
  [[ "$CA_MODEL" == env-model && "$DASHSCOPE_API_KEY" == env-key ]]
  [[ "$CA_API_BASE_URL" == https://dashscope.aliyuncs.com/apps/anthropic ]]
)

for harness in openclaw hermes codex claude-code opencode qodercli pi dsh; do
  (
    init_defaults
    parse_args --harness "$harness" --api-base-url https://inference.example/v1 \
      --api-key test-inference-key --model 'custom/model' --work-dir "$task_tmp/$harness" \
      --state-dir "$task_tmp/state" --cosign-key "$task_tmp/cosign.key" \
      --image-release "$task_tmp/release.json" --trusted-key "$task_tmp/publisher.pem"
    normalize_workspace_paths
    resolve_harness_options
    apply_target_defaults
    validate_options
    if [[ "$harness" == qodercli ]]; then
      unset DASHSCOPE_API_KEY
    else
      ensure_bailian_credentials
    fi
    if [[ "$harness" == openclaw ]]; then prepare_openclaw_specs; else prepare_harness_specs; fi
    "$CA_BIN" spec validate --spec "$CA_SPEC_PATH"
  )
done

# Exercise automatic publication and cached reuse with real init; replace only
# the image build/cloud operations so this test does not allocate cloud resources.
(
  init_defaults
  parse_args --harness codex --api-key test-inference-key \
    --work-dir "$task_tmp/automatic" --state-dir "$task_tmp/state" \
    --cosign-key "$task_tmp/cosign.key"
  normalize_workspace_paths
  resolve_harness_options
  apply_target_defaults
  ensure_bailian_credentials
  ca_cmd() {
    case "$1" in
      init) "$CA_BIN" "$@" ;;
      build) printf 'build\n' >> "$task_tmp/cloud-operations" ;;
      image)
        printf '%s\n' "$1 $2" >> "$task_tmp/cloud-operations"
        if [[ "$2" == export ]]; then cp "$task_tmp/release.json" "$CA_IMAGE_RELEASE"; fi
        ;;
      *) return 1 ;;
    esac
  }
  prepare_harness_specs
  CA_IMAGE_RELEASE=""
  prepare_harness_specs
  [[ "$(cat "$task_tmp/cloud-operations")" == $'build\nimage publish\nimage export' ]]
  [[ -s "$CA_TRUSTED_KEY" && -s "$CA_WORK_DIR/allinone-builder/build.yaml" ]]
  [[ ! -d "$CA_WORK_DIR/allinone-builder/secrets" ]]
)

python3.11 - "$task_tmp" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
claw = json.loads((root / 'openclaw/openclaw/openclaw.json').read_text())
assert claw['models']['providers']['bailian']['baseUrl'] == 'https://inference.example/v1'
assert claw['agents']['defaults']['model']['primary'] == 'bailian/custom/model'
assert claw['models']['providers']['bailian']['apiKey'] == 'test-inference-key'
for harness in ['hermes', 'codex', 'claude-code', 'opencode', 'qodercli', 'pi', 'dsh']:
    secrets = root / harness / f'allinone-{harness}' / 'secrets'
    runtime = json.loads((secrets / 'runtime.json').read_text())
    assert (runtime['harness'], runtime['mode']) == (harness, 'cli')
    if harness == 'codex':
        config = (secrets / 'config.toml').read_text()
        assert 'base_url = "https://inference.example/v1"' in config
        assert 'model = "custom/model"' in config
        assert runtime['environment']['OPENAI_API_KEY'] == 'test-inference-key'
    elif harness == 'claude-code':
        assert runtime['environment']['ANTHROPIC_BASE_URL'] == 'https://inference.example/v1'
        assert runtime['environment']['ANTHROPIC_MODEL'] == 'custom/model'
        assert runtime['environment']['ANTHROPIC_AUTH_TOKEN'] == 'test-inference-key'
    elif harness == 'opencode':
        config = json.loads((secrets / 'opencode.json').read_text())
        assert config['provider']['bailian']['options'] == {
            'baseURL': 'https://inference.example/v1', 'apiKey': 'test-inference-key'}
        assert config['model'] == 'bailian/custom/model'
    elif harness == 'hermes':
        config = (secrets / 'config.yaml').read_text()
        assert 'provider: custom' in config
        assert 'base_url: https://inference.example/v1' in config
        assert 'api_key: test-inference-key' in config
        assert 'default: custom/model' in config
    elif harness == 'pi':
        models = json.loads((secrets / 'pi-models.json').read_text())
        provider = models['providers']['inference']
        assert provider['baseUrl'] == 'https://inference.example/v1'
        assert provider['models'] == [{'id': 'custom/model'}]
        assert provider['apiKey'] == '$CA_INFERENCE_API_KEY'
        settings = json.loads((secrets / 'pi-settings.json').read_text())
        assert settings['defaultModel'] == 'custom/model'
        assert runtime['environment']['CA_INFERENCE_API_KEY'] == 'test-inference-key'
    elif harness == 'dsh':
        config = (secrets / 'cordis.patch.yml').read_text()
        assert 'baseURL: https://inference.example/v1' in config
        assert 'model: custom/model' in config
        assert 'apiKeyEnv: CA_INFERENCE_API_KEY' in config
        assert runtime['environment']['CA_INFERENCE_API_KEY'] == 'test-inference-key'
    else:
        assert runtime['environment'] == {}
print('Default OpenClaw, aliases, interactive choices and eight native harness configurations passed')
PY
