"""The shared producer only builds fixtures that the matrix consumes."""
import base64
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import publish_fixtures
import suite


class PublisherConsumersTest(unittest.TestCase):
    def test_inference_and_allinone_select_only_their_own_producer(self):
        manifest = json.loads((suite.ROOT / 'tools/e2e/suite.json').read_text())
        cases = {case['id']: case for case in manifest['cases']}
        for name, case in cases.items():
            if not name.startswith('inference-'):
                continue
            with self.subTest(case=name):
                selected = suite.select_cases(cases, [], [name])
                self.assertIn('prepare-inference-images', selected)
                self.assertNotIn('publish-reuse-images', selected)
        selected = suite.select_cases(cases, [], ['allinone-challenge'])
        self.assertIn('publish-reuse-images', selected)
        self.assertNotIn('prepare-inference-images', selected)
        inference = {name: case for name, case in cases.items() if name.startswith('inference-')}
        self.assertEqual(set(inference), {'inference-alb', 'inference-nlb-rats', 'inference-reuse-nlb'})
        self.assertTrue(all(case['environment']['E2E_IMAGE_VARIANT'] == 'debug' and
                            case['environment']['E2E_REUSE_IMAGE'] == '1' for case in inference.values()))
        self.assertEqual(cases['inference-alb']['environment']['E2E_ONECLICK'], '1')
        self.assertEqual(cases['inference-reuse-nlb']['environment']['E2E_SERVICE_ISOLATION'], '1')
        self.assertEqual(cases['inference-reuse-nlb']['environment']['E2E_ATTESTATION_MODE'], 'trustee')
        full = suite.select_cases(cases, ['full'], [])
        self.assertNotIn('openclaw-vllm-release', full)
        self.assertEqual(sum(case.get('gpu_instances', 0) for case in full.values()), 8)

    def test_external_allinone_consumer_needs_no_inference_or_model_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, work = root / 'source', root / 'consumer'
            work.mkdir()
            for name in ['allinone', 'allinone-hardened']:
                fixture = source / name
                fixture.mkdir(parents=True)
                payload = {'images': [{'region': 'cn-beijing', 'image_id': 'm-published'}],
                           'manifest': {'manifest_digest': 'sha256:' + 'a' * 64}}
                (fixture / 'image-release.json').write_text(json.dumps({
                    'payload': base64.b64encode(json.dumps(payload).encode()).decode()}))
                (fixture / 'publisher.pem').write_text('public test fixture')
            (source / 'allinone-publisher-state.json').write_text('{}')
            with patch.dict(os.environ, E2E_PUBLISHED_FIXTURES=str(source), E2E_WORK_DIR=str(work)), \
                    patch.object(sys, 'argv', ['publish_fixtures.py']):
                publish_fixtures.main()
            self.assertFalse((work / 'inference-debug').exists())
            result = json.loads((work / 'case-result.json').read_text())
            self.assertTrue(result['passed'])
            self.assertTrue(result['checks']['external_signed_fixtures'])

    def test_new_producer_builds_and_publishes_release_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            work = root / 'producer'
            work.mkdir()
            state = work / 'state'
            state.mkdir()
            commands, published = [], {}

            class FakeCase:
                def __init__(self):
                    self.work, self.state, self.region = work, state, 'cn-beijing'
                    self.checks = {}

                def __enter__(self):
                    return self

                def __exit__(self, *_):
                    return False

                def command(self, label, *arguments, **_):
                    commands.append((label, arguments))
                    if arguments[:2] == ('init', 'allinone'):
                        service = arguments[arguments.index('--service-id') + 1]
                        output = Path(arguments[arguments.index('--output-dir') + 1]) / service
                        output.mkdir(parents=True)
                        (output / 'build.yaml').write_text('service:\n  id: ' + service + '\n  ssh: true\nbuild:\n  image_name: allinone\n')
                        (output / 'runtime-contract.json').write_text(json.dumps({
                            'capabilities': ['resources-v1', 'instance-ssh-v1', 'instance-ssh-credentials-v1']}))
                    elif arguments[0] == 'build':
                        spec = Path(arguments[arguments.index('--spec') + 1])
                        service = spec.parent.name
                        directory = state / 'services' / service
                        directory.mkdir(parents=True)
                        rendered = directory / 'shelter.yaml'
                        rendered.write_text('security:\n  harden:\n    enabled: true\n    mode: '
                                            + ('full' if service == 'allinone-hardened-builder' else 'partial') + '\n')
                        contract = json.loads((spec.parent / 'runtime-contract.json').read_text())
                        contract['image_variant'] = 'release'
                        (directory / 'manifest.json').write_text(json.dumps({
                            'shelter_config': str(rendered), 'runtime_contract': contract}))
                    elif arguments[:2] == ('image', 'publish'):
                        service = arguments[2]
                        variant = arguments[arguments.index('--variant') + 1]
                        published[service] = {'build': {'published': {variant: {
                            'region': 'cn-beijing', 'variant': variant, 'image_id': 'm-test-' + service,
                        }}}}
                    elif arguments[:2] == ('image', 'export'):
                        if '--account-id' in arguments:
                            raise AssertionError('image export no longer accepts --account-id')
                        Path(arguments[arguments.index('--output') + 1]).write_text('{}')

                def local_state(self, service):
                    return published[service]

            case = FakeCase()

            def fake_run(command, *_args, **_kwargs):
                if '-out' in command:
                    Path(command[command.index('-out') + 1]).write_text('test key fixture')
                return SimpleNamespace(stdout=b'{"AccountId":"test-account"}')

            environment = dict(os.environ, E2E_MODEL_KEY_FILE='/test/model-key', E2E_MODEL_OSS_BUCKET='test-models')
            environment.pop('E2E_PUBLISHED_FIXTURES', None)
            with patch.dict(os.environ, environment, clear=True), patch.object(sys, 'argv', ['publish_fixtures.py']), \
                    patch.object(publish_fixtures, 'ROOT', root / 'repo'), \
                    patch.object(publish_fixtures, 'Case', return_value=case), \
                    patch.object(publish_fixtures, 'run', side_effect=fake_run):
                publish_fixtures.main()
            init = next(args for _, args in commands if args[0] == 'init')
            self.assertEqual(init[:2], ('init', 'allinone'))
            self.assertEqual(init[init.index('--stage') + 1], 'build')
            self.assertEqual(init[init.index('--reference-values') + 1], 'rekor')
            self.assertEqual(init[init.index('--cosign-key') + 1], work / 'cosign.key')
            self.assertEqual(len([args for _, args in commands if args[0] == 'build']), 2)
            build = next(args for _, args in commands if args[0] == 'build')
            self.assertEqual(build[build.index('--spec') + 1], work / 'init/allinone-builder/build.yaml')
            publications = [args for _, args in commands if args[:2] == ('image', 'publish')]
            self.assertEqual(len(publications), 2)
            self.assertEqual({args[2] for args in publications}, {'allinone-builder', 'allinone-hardened-builder'})
            self.assertTrue(all(args[args.index('--variant') + 1] == 'release' for args in publications))
            self.assertTrue(case.checks['signed_images_published'])
            self.assertTrue(case.checks['full_hardening_release_built'])
            strict = work / 'init/allinone-hardened-builder/build.yaml'
            self.assertIn('  ssh: false', strict.read_text())
            self.assertIn('  image_name: allinone-hardened', strict.read_text())
            self.assertIn('  ssh: true', (work / 'init/allinone-builder/build.yaml').read_text())


if __name__ == '__main__':
    unittest.main()
