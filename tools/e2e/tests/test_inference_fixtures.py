"""Shared builds keep debug keys local and publication ownership durable."""
import base64
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_fixtures
import inference_case
import publish_fixtures


class InferenceFixturesTest(unittest.TestCase):
    def test_fresh_producer_applies_the_mirror_before_build_and_keeps_epel(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = MagicMock(work=root, region='cn-beijing')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            case.checks = {}
            events = []
            spec = root / 'init/vllm-inference/vllm-inference.yaml'
            def command(label, *arguments, **options):
                if arguments[0] == 'init':
                    spec.parent.mkdir(parents=True)
                    spec.write_text('build:\n  image_name: fixture\n  repos:\n'
                        '    - name: epel\n      baseurl: https://epel.example/packages/\n'
                        '      gpgkey: https://epel.example/key\n  scripts: [./install.sh]\n')
                    events.append('init')
                elif arguments[0] == 'build':
                    contents = spec.read_text()
                    self.assertEqual(contents.count('name: alinux-'), 5)
                    self.assertIn('https://mirror.example/alinux/3/os/$basearch/', contents)
                    self.assertIn('baseurl: https://epel.example/packages/', contents)
                    self.assertIn('gpgkey: https://epel.example/key', contents)
                    events.append('build')
            case.command.side_effect = command
            environment = {'E2E_MODEL_KEY_FILE': '/fixture/model-key', 'E2E_MODEL_OSS_BUCKET': 'fixture-models',
                           'E2E_MATRIX_REPO_MIRROR': 'https://mirror.example/alinux'}
            with patch.dict(os.environ, environment, clear=True), patch.object(sys, 'argv', ['inference_fixtures.py']), \
                    patch.object(inference_fixtures, 'Case', return_value=case), \
                    patch.object(inference_fixtures, 'run'), \
                    patch.object(inference_fixtures, 'record_debug_identity'), \
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True), \
                    patch.object(inference_fixtures, 'publish_variant'), \
                    patch.object(inference_fixtures, 'cleanup_publications'):
                inference_fixtures.main()
            self.assertEqual(events, ['init', 'build'])

    def test_producer_builds_and_publishes_debug_once(self):
        with tempfile.TemporaryDirectory() as directory:
            case = MagicMock(work=Path(directory), region='cn-beijing')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            case.checks = {}
            environment = {'E2E_MODEL_KEY_FILE': '/fixture/model-key', 'E2E_MODEL_OSS_BUCKET': 'fixture-models'}
            with patch.dict(os.environ, environment, clear=True), patch.object(sys, 'argv', ['inference_fixtures.py']), \
                    patch.object(inference_fixtures, 'Case', return_value=case), \
                    patch.object(inference_fixtures, 'run', return_value=SimpleNamespace(stdout=b'{"AccountId":"test"}')), \
                    patch.object(inference_fixtures, 'record_debug_identity'), \
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True) as verify, \
                    patch.object(inference_fixtures, 'publish_variant') as publish:
                inference_fixtures.main()
            commands = [call.args for call in case.command.call_args_list]
            self.assertEqual(len([args for args in commands if args[1] == 'build']), 1)
            init = next(args for args in commands if args[1] == 'init')
            self.assertEqual(init[init.index('--vllm-build-variants') + 1], 'debug')
            self.assertEqual(init[init.index('--vllm-served-model-name') + 1], inference_case.MODEL)
            self.assertEqual(verify.call_args.args[-1], ['debug'])
            self.assertEqual(publish.call_count, 1)
            self.assertEqual(publish.call_args.args[3], 'debug')

    def test_resumed_publication_reverifies_debug_without_build_or_new_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            case = MagicMock(work=Path(directory), region='cn-beijing')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            case.checks = {}
            with patch.dict(os.environ, {'E2E_MODEL_KEY_FILE': '/fixture/key', 'E2E_MODEL_OSS_BUCKET': 'fixture-models'}, clear=True), \
                    patch.object(sys, 'argv', ['inference_fixtures.py', '--resume']), \
                    patch.object(inference_fixtures, 'Case', return_value=case), \
                    patch.object(inference_fixtures, 'resume_producer') as resume, \
                    patch.object(inference_fixtures, 'run', return_value=SimpleNamespace(stdout=b'{"AccountId":"test"}')) as run, \
                    patch.object(inference_fixtures, 'record_debug_identity'), \
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True) as verify, \
                    patch.object(inference_fixtures, 'publish_variant') as publish:
                inference_fixtures.main()
            resume.assert_called_once_with(case)
            case.command.assert_not_called()
            self.assertEqual(verify.call_args.args[-1], ['debug'])
            self.assertEqual(run.call_count, 1)
            self.assertEqual(publish.call_count, 1)

    def test_failed_publication_retains_verified_local_builds_for_resume(self):
        with tempfile.TemporaryDirectory() as directory:
            case = MagicMock(work=Path(directory), region='cn-beijing')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            case.checks = {}
            with patch.dict(os.environ, {'E2E_MODEL_KEY_FILE': '/fixture/key', 'E2E_MODEL_OSS_BUCKET': 'fixture-models'}, clear=True), \
                    patch.object(sys, 'argv', ['inference_fixtures.py']), \
                    patch.object(inference_fixtures, 'Case', return_value=case), \
                    patch.object(inference_fixtures, 'run', return_value=SimpleNamespace(stdout=b'{"AccountId":"test"}')), \
                    patch.object(inference_fixtures, 'record_debug_identity'), \
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True), \
                    patch.object(inference_fixtures, 'publish_variant', side_effect=RuntimeError('upload failed')), \
                    patch.object(inference_fixtures, 'cleanup') as remove_local, \
                    patch.object(inference_fixtures, 'cleanup_publications') as cleanup:
                with self.assertRaisesRegex(RuntimeError, 'upload failed'):
                    inference_fixtures.main()
            cleanup.assert_called_once_with(case)
            remove_local.assert_not_called()

    def test_external_runner_copies_only_debug_envelope_and_public_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'producer/inference-debug'
            source.mkdir(parents=True)
            payload = {'images': [{'region': 'cn-beijing', 'account_id': 'test'}],
                       'manifest': {'manifest_digest': 'sha256:' + 'a' * 64}}
            (source / 'image-release.json').write_text(json.dumps({
                'payload': base64.b64encode(json.dumps(payload).encode()).decode()}))
            (source / 'publisher.pem').write_text('test public key')
            (source / 'private.key').write_text('never distribute this')
            work = root / 'consumer'
            with patch.dict(os.environ, E2E_INFERENCE_FIXTURES=str(source.parent), E2E_WORK_DIR=str(work)), \
                    patch.object(sys, 'argv', ['inference_fixtures.py']), \
                    patch.object(inference_fixtures, 'run') as run:
                inference_fixtures.main()
            run.assert_not_called()
            self.assertEqual(sorted(p.name for p in (work / 'inference-debug').iterdir()),
                             ['image-release.json', 'publisher.pem'])
            self.assertTrue(json.loads((work / 'case-result.json').read_text())['passed'])
            self.assertFalse((work / 'debug-key-path.json').exists())
            self.assertFalse((work / 'state/services').exists())

    def test_debug_uses_an_original_key_reference_without_changing_consumer_state(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
            root = Path(directory)
            producer = root / 'prepare-inference-images'
            key = producer / 'state/services/vllm-inference/secrets/debug_ssh'
            key.parent.mkdir(parents=True)
            key.write_text('original private key')
            case = SimpleNamespace(work=producer, suite=root, state=root / 'consumer/state',
                local_state=lambda _: {'build': {'debug_ssh': {'private_key': str(key)}}})
            original_state = case.state
            inference_fixtures.record_debug_identity(case)
            self.assertEqual(inference_fixtures.debug_identity(case), key)
            self.assertEqual(case.state, original_state)
            self.assertFalse(original_state.exists())
            self.assertEqual(json.loads((producer / 'debug-key-path.json').read_text()), {'private_key': str(key)})

    def test_external_debug_requires_an_explicit_original_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            producer = root / 'prepare-inference-images'
            producer.mkdir()
            key = root / 'original-debug-key'
            key.write_text('original private key')
            (producer / 'debug-key-path.json').write_text(json.dumps({'private_key': str(key)}))
            case = SimpleNamespace(suite=root)
            with patch.dict(os.environ, {'E2E_INFERENCE_FIXTURES': '/external'}, clear=True):
                with self.assertRaisesRegex(RuntimeError, 'E2E_INFERENCE_DEBUG_KEY_FILE'):
                    inference_fixtures.debug_identity(case)
                with patch.dict(os.environ, E2E_INFERENCE_DEBUG_KEY_FILE=str(key)):
                    self.assertEqual(inference_fixtures.debug_identity(case), key)

    def test_missing_debug_key_fails_before_any_cloud_or_operator_action(self):
        with tempfile.TemporaryDirectory() as directory:
            case = MagicMock(suite=Path(directory), state=Path(directory) / 'consumer-state')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            for key in [None, str(Path(directory) / 'missing-key')]:
                environment = {'E2E_REUSE_IMAGE': '1', 'E2E_IMAGE_VARIANT': 'debug'}
                if key:
                    environment['E2E_INFERENCE_DEBUG_KEY_FILE'] = key
                with self.subTest(key=key), patch.dict(os.environ, environment, clear=True), \
                        patch.object(inference_case, 'Case', return_value=case):
                    with self.assertRaisesRegex(RuntimeError, 'SSH key'):
                        inference_case.main()
            case.operator.assert_not_called()
            case.api.assert_not_called()
            case.trustee.assert_not_called()
            case.command.assert_not_called()

    def test_export_failure_still_has_durable_publication_and_snapshot_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = SimpleNamespace(work=root, region='cn-beijing', command=Mock(),
                local_state=lambda _: {'build': {'published': {'release': {
                    'region': 'cn-beijing', 'variant': 'release', 'image_id': 'm-owned'}}}})
            def command(label, *_args, **_kwargs):
                if label.startswith('export-'):
                    self.assertEqual(json.loads((root / 'published-images.json').read_text())[0]['image_id'], 'm-owned')
                    raise RuntimeError('export failed')
            case.command.side_effect = command
            with patch.object(publish_fixtures, 'record_publication_snapshots') as inventory:
                with self.assertRaisesRegex(RuntimeError, 'export failed'):
                    publish_fixtures.publish_variant(case, 'inference', root / 'spec', 'release', root / 'release', [])
            inventory.assert_called_once_with(case)

    def test_debug_deployment_must_finish_before_removing_build_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'services/vllm-inference/state.json'
            state.parent.mkdir(parents=True)
            case = SimpleNamespace(state=root, work=root, command=Mock())
            state.write_text('{"phase":"active"}')
            with patch.object(inference_fixtures, 'cleanup_publications'):
                with self.assertRaisesRegex(RuntimeError, 'debug deployment'):
                    inference_fixtures.cleanup(case)
                case.command.assert_not_called()
                state.write_text('{"phase":"deleted"}')
                inference_fixtures.cleanup(case)
            self.assertEqual(case.command.call_args.args[1:4], ('image', 'rm', 'vllm-inference'))


if __name__ == '__main__':
    unittest.main()
