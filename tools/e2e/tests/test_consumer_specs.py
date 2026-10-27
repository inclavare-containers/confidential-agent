"""Exercise generated consumption specs against the installed CLI schema."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/e2e'))
from fixtures.allinone import generate

module_spec = importlib.util.spec_from_file_location('inference_case_specs', ROOT / 'tools/e2e/inference_case.py')
inference = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(inference)
import platform_case
reuse_spec = importlib.util.spec_from_file_location('reuse_schema_checks', ROOT / 'tools/e2e/allinone-reuse.py')
reuse = importlib.util.module_from_spec(reuse_spec)
reuse_spec.loader.exec_module(reuse)


@unittest.skipUnless(os.environ.get('CA_BIN'), 'requires CA_BIN from the tested source')
class ConsumerSpecTest(unittest.TestCase):
    def selection(self, root):
        release = root / 'image-release.json'
        release.write_text(json.dumps({'payload': base64.b64encode(json.dumps({
            'images': [{'region': 'cn-beijing', 'account_id': '1234567890'}],
            'manifest': {'manifest_digest': 'sha256:' + 'a' * 64}}).encode()).decode()}))
        public = root / 'publisher.pem'
        public.write_text('schema-only public fixture\n')
        return {'release': str(release), 'trusted_key': str(public),
                'manifest': 'sha256:' + 'a' * 64, 'account_id': '1234567890'}

    def command(self, *args):
        result = subprocess.run([os.environ['CA_BIN'], *map(str, args)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_all_harness_consumers_validate_without_producer_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = self.selection(root)
            with patch.dict(os.environ, {'DASHSCOPE_API_KEY': 'fixture-key',
                    'QODER_PERSONAL_ACCESS_TOKEN': 'fixture-account', 'E2E_QODER_MODEL': 'fixed-model'}):
                for mode in ['challenge', 'trustee']:
                    for fixture in generate(root / mode, image, mode, 'cn-beijing', mode):
                        with self.subTest(mode=mode, harness=fixture.name):
                            self.command('spec', 'validate', '--spec', fixture / 'appspec.json')
            self.assertFalse(list(root.rglob('*.key')), 'consumer schema validation must not need signing keys')

    def test_inference_consumers_validate_for_variants_and_replicas(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for variant in ['release', 'debug']:
                published = root / 'prepare-inference-images' / ('inference-' + variant)
                published.mkdir(parents=True)
                self.selection(published)
            model_key = root / 'model-key'
            model_key.write_text('fixture-model-key')
            with patch.dict(os.environ, {'E2E_MODEL_KEY_FILE': str(model_key),
                    'E2E_MODEL_OSS_BUCKET': 'fixture:/encrypted-model/'}):
                for variant in ['release', 'debug']:
                    for mode in ['challenge', 'trustee']:
                        for replicas, kind, transport in [(1, 'nlb', 'ohttp'), (2, 'alb', 'ohttp'), (2, 'nlb', 'rats-tls')]:
                            with self.subTest(variant=variant, mode=mode, replicas=replicas, transport=transport):
                                work = root / f'{variant}-{mode}-{replicas}-{kind}'
                                work.mkdir()
                                class LocalCase:
                                    region = 'cn-beijing'
                                    suite = root
                                    def command(_, label, *args):
                                        return self.command(*args)
                                case = LocalCase()
                                case.work = work
                                spec = inference.prepare(case, 'schema-probe', replicas, kind,
                                                         transport, mode, True, variant)
                                self.assertFalse((work / 'cosign.key').exists())
                                self.command('spec', 'validate', '--spec', spec)

    def test_fresh_inference_specs_validate_with_declarative_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model_key = root / 'model-key'
            model_key.write_text('fixture-model-key')
            with patch.dict(os.environ, {'E2E_MODEL_KEY_FILE': str(model_key),
                    'E2E_MODEL_OSS_BUCKET': 'fixture:/encrypted-model/'}):
                for variant in ['release', 'debug']:
                    for mode in ['challenge', 'trustee']:
                        for kind, transport in [('alb', 'ohttp'), ('nlb', 'rats-tls')]:
                            with self.subTest(variant=variant, mode=mode, kind=kind):
                                work = root / f'{variant}-{mode}-{kind}'
                                work.mkdir()
                                class LocalCase:
                                    region = 'cn-beijing'
                                    suite = root
                                    def command(_, label, *args):
                                        return self.command(*args)
                                case = LocalCase()
                                case.work = work
                                spec = inference.prepare(case, 'fresh-schema-probe', 2, kind,
                                                         transport, mode, False, variant)
                                self.command('spec', 'validate', '--spec', spec)

    def test_platform_fixtures_have_live_mesh_peers_and_valid_specs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.dict(os.environ, {'DASHSCOPE_API_KEY': 'fixture-key'}):
                selected, extra = platform_case.prepare_fixtures(root, self.selection(root), 'cn-beijing')
            runtime = runpy.run_path(str(ROOT / 'examples/allinone/files/cai-agent'))
            self.assertEqual([fixture.name for fixture in selected], ['codex', 'openclaw', 'hermes'])
            for fixture in [*selected, extra]:
                spec = json.loads((fixture / 'appspec.json').read_text())
                port = runtime['load_configuration'](fixture / 'runtime.json')[2]
                context = fixture / 'application-runtime.json'
                context.write_text(json.dumps({'schema_version': 'confidential-agent/application-runtime/v1',
                                               'ports': spec['service']['ports']}))
                runtime['validate_declared_ports'](port, context)
            consumers = []
            for fixture in selected:
                spec = json.loads((fixture / 'appspec.json').read_text())
                consumers.append(SimpleNamespace(spec=spec, service=spec['service']['id']))
                self.command('spec', 'validate', '--spec', fixture / 'appspec.json')
            extra_spec = json.loads((extra / 'appspec.json').read_text())
            self.command('spec', 'validate', '--spec', extra / 'appspec.json')
            peers = reuse.stable_endpoint_peers(consumers, extra_spec)
            self.assertEqual(len(peers), 2)
            for fixture in [*selected[1:], extra]:
                self.assertIsNone(json.loads((fixture / 'probe.json').read_text())['connect_http_path'])


if __name__ == '__main__':
    unittest.main()
