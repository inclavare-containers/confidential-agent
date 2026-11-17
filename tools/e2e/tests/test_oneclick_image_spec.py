"""The one-click entry point can validate and deploy a signed-image spec."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]


class OneClickImageSpecTest(unittest.TestCase):
    def test_signed_image_skips_init_and_build_but_still_requires_spec_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = root / 'consumer.json'
            spec.write_text(json.dumps({'service': {'id': 'vllm-inference'},
                                       'deploy': {'image': {'release': 'signed-release.json'}}}))
            script = '''
set -euo pipefail
source "$ROOT_DIR/one-click/lib/openclaw.sh"
ensure_gateway_token() { :; }
ensure_cosign_key() { touch "$TEST_WORK/unexpected-key"; return 99; }
run_confidential_agent_init() { touch "$TEST_WORK/unexpected-init"; return 99; }
log() { :; }
ca_cmd() { [[ "$1 $2 $3 $4" == "spec validate --spec $CA_IMAGE_SPEC" ]] && [[ "$REJECT_SPEC" == 0 ]]; }
prepare_vllm_inference_specs
[[ "$CA_SKIP_BUILD" == 1 && "$CA_SPEC_PATH" == "$CA_IMAGE_SPEC" && "$CA_PROJECT_DIR" == "$TEST_WORK" ]]
'''
            for rejected in [False, True]:
                environment = dict(os.environ, ROOT_DIR=str(ROOT), TEST_WORK=str(root),
                    CA_IMAGE_SPEC=str(spec), CA_SKIP_BUILD='0', REJECT_SPEC=str(int(rejected)))
                result = subprocess.run(['bash', '-c', script], env=environment, capture_output=True)
                self.assertEqual(result.returncode == 0, not rejected, result.stderr)
            self.assertFalse(list(root.glob('unexpected-*')))

    def test_wrong_service_and_non_image_specs_fail_before_deployment(self):
        with tempfile.TemporaryDirectory() as directory:
            spec = Path(directory) / 'consumer.json'
            for value in [{'service': {'id': 'another-service'}, 'deploy': {'image': {'release': 'signed'}}},
                          {'service': {'id': 'vllm-inference'}, 'build': {}}]:
                spec.write_text(json.dumps(value))
                result = subprocess.run(['bash', '-c', '''
set -euo pipefail
source "$ROOT_DIR/one-click/lib/openclaw.sh"
ensure_gateway_token() { :; }
ca_cmd() { exit 99; }
prepare_vllm_inference_specs
'''], env=dict(os.environ, ROOT_DIR=str(ROOT), CA_IMAGE_SPEC=str(spec)), capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotEqual(result.returncode, 99)


if __name__ == '__main__':
    unittest.main()
