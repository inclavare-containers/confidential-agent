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
digest="sha256:$(sha256sum "$task_tmp/release.json" | cut -d' ' -f1)"
printf 'cosign fixture\n' > "$task_tmp/cosign.key"
export CA_SLSA_GENERATOR=/bin/true

(
  init_defaults
  resolve_harness_options
  apply_target_defaults
  [[ "$CA_HARNESS" == openclaw && "$CA_MODEL" == qwen4-max ]]
  [[ "$CA_SERVICE_ID" == openclaw && "$CA_SPEC_NAME" == instance.yaml && "$CA_CONNECT_NAME" == connect ]]
  [[ "$CA_API_BASE_URL" == https://dashscope.aliyuncs.com/compatible-mode/v1 ]]
  parse_args --bailian-api-key legacy-key --bailian-model legacy-model
  ensure_bailian_credentials
  [[ "$DASHSCOPE_API_KEY" == legacy-key && "$CA_MODEL" == legacy-model ]]
)
(
  CA_NON_INTERACTIVE=0
  init_defaults
  resolve_harness_options <<< $'claudecode\nclaude-code\nchosen-model\nhttps://inference.example/anthropic'
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

# Exercise the public entry point without running deployment or host setup.
(
  run_deploy_cpu() { :; }
  run_cleanup() { :; }
  run_install_only() { :; }
  expect_usage_error() {
    local expected="$1" output
    shift
    if output="$(main "$@" --state-dir "$task_tmp/options-state" --work-dir "$task_tmp/options-work" 2>&1)"; then
      printf 'expected error: %s\n' "$expected" >&2
      exit 1
    fi
    [[ "$output" == *"$expected"* ]]
  }
  expect_usage_error 'unsupported harness' --harness claudecode
  expect_usage_error 'unsupported harness' cleanup --harness claudecode
  CA_HARNESS=claudecode expect_usage_error 'unsupported harness'
  expect_usage_error '--disable-pep applies only to OpenClaw' --harness codex --disable-pep
  expect_usage_error '--enable-dingtalk applies only to OpenClaw' --harness codex --enable-dingtalk
  expect_usage_error 'CLI harnesses require --replicas 1' --harness codex --access ssh --replicas 2
  expect_usage_error '--image-release requires --release-digest' --harness codex --image-release "$task_tmp/release.json"
  expect_usage_error 'unsupported access' --harness pi --access web
  for mode in cleanup install-only; do
    (main "$mode" --harness codex --disable-pep --enable-dingtalk --replicas 2 \
      --state-dir "$task_tmp/options-state" --work-dir "$task_tmp/options-work")
  done
)

for harness in openclaw hermes codex claude-code opencode qodercli pi dsh; do
  (
    init_defaults
    parse_args --harness "$harness" --api-base-url https://inference.example/v1 \
      --api-key test-inference-key --model 'custom/model' --work-dir "$task_tmp/$harness" \
      --state-dir "$task_tmp/state" --cosign-key "$task_tmp/cosign.key"
    parse_args --image-release "$task_tmp/release.json" --release-digest "$digest" --skip-deploy
    normalize_workspace_paths
    resolve_harness_options
    apply_target_defaults
    validate_options
    if [[ "$harness" == qodercli ]]; then
      unset DASHSCOPE_API_KEY
    else
      ensure_bailian_credentials
    fi
    prepare_cpu_specs
    "$CA_BIN" spec validate --spec "$CA_SPEC_PATH"
  )
done

python3.11 - "$task_tmp" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
claw = json.loads((root / 'openclaw/openclaw/secrets/openclaw.json').read_text())
assert claw['models']['providers']['bailian']['baseUrl'] == 'https://inference.example/v1'
assert claw['agents']['defaults']['model']['primary'] == 'bailian/custom/model'
assert claw['models']['providers']['bailian']['apiKey'] == 'test-inference-key'
for harness in ['hermes', 'codex', 'claude-code', 'opencode', 'qodercli', 'pi', 'dsh']:
    secrets = root / harness / harness / 'secrets'
    runtime = json.loads((secrets / 'runtime.json').read_text())
    modes = {'codex':'app-server', 'hermes':'gateway', 'dsh':'web', 'opencode':'web'}
    assert (runtime['harness'], runtime['mode']) == (harness, modes.get(harness, 'cli'))
    if harness == 'codex':
        config = (secrets / 'config.toml').read_text()
        assert 'base_url = "https://inference.example/v1"' in config
        assert 'model = "custom/model"' in config
        assert (secrets / 'app-server-token').read_text().strip()
        assert runtime['environment']['OPENAI_API_KEY'] == 'test-inference-key'
    elif harness == 'claude-code':
        assert runtime['environment']['ANTHROPIC_BASE_URL'] == 'https://inference.example/v1'
        assert runtime['environment']['ANTHROPIC_MODEL'] == 'custom/model'
        assert runtime['environment']['ANTHROPIC_AUTH_TOKEN'] == 'test-inference-key'
        for variable in ['ANTHROPIC_DEFAULT_HAIKU_MODEL', 'ANTHROPIC_DEFAULT_SONNET_MODEL',
                         'ANTHROPIC_DEFAULT_OPUS_MODEL', 'CLAUDE_CODE_SUBAGENT_MODEL']:
            assert runtime['environment'][variable] == 'custom/model'
        assert runtime['environment']['DISABLE_AUTOUPDATER'] == '1'
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
        assert runtime['environment']['API_SERVER_KEY']
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
