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
    def test_producer_builds_both_variants_in_one_command_and_only_publishes_release(self):
        with tempfile.TemporaryDirectory() as directory:
            case = MagicMock(work=Path(directory), region='cn-beijing')
            case.__enter__.return_value = case
            case.__exit__.return_value = False
            case.checks = {}
            environment = {'E2E_MODEL_KEY_FILE': '/fixture/model-key', 'E2E_MODEL_OSS_BUCKET': 'fixture-models'}
            with patch.dict(os.environ, environment, clear=True), patch.object(sys, 'argv', ['inference_fixtures.py']), \
                    patch.object(inference_fixtures, 'Case', return_value=case), \
                    patch.object(inference_fixtures, 'run', return_value=SimpleNamespace(stdout=b'{"AccountId":"test"}')), \
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True) as verify, \
                    patch.object(inference_fixtures, 'publish_variant') as publish:
                inference_fixtures.main()
            commands = [call.args for call in case.command.call_args_list]
            self.assertEqual(len([args for args in commands if args[1] == 'build']), 1)
            init = next(args for args in commands if args[1] == 'init')
            self.assertEqual(init[init.index('--vllm-build-variants') + 1], 'release,debug')
            self.assertEqual(init[init.index('--vllm-served-model-name') + 1], inference_case.MODEL)
            self.assertEqual(verify.call_args.args[-1], ['release', 'debug'])
            self.assertEqual(publish.call_count, 1)
            self.assertEqual(publish.call_args.args[3], 'release')

    def test_resumed_publication_reverifies_both_variants_without_build_or_new_keys(self):
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
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True) as verify, \
                    patch.object(inference_fixtures, 'publish_variant') as publish:
                inference_fixtures.main()
            resume.assert_called_once_with(case)
            case.command.assert_not_called()
            self.assertEqual(verify.call_args.args[-1], ['release', 'debug'])
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
                    patch.object(inference_fixtures, 'verify_resumable_build', return_value=True), \
                    patch.object(inference_fixtures, 'publish_variant', side_effect=RuntimeError('upload failed')), \
                    patch.object(inference_fixtures, 'cleanup') as remove_local, \
                    patch.object(inference_fixtures, 'cleanup_publications') as cleanup:
                with self.assertRaisesRegex(RuntimeError, 'upload failed'):
                    inference_fixtures.main()
            cleanup.assert_called_once_with(case)
            remove_local.assert_not_called()

    def test_external_runner_copies_only_release_envelope_and_public_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'producer/inference-release'
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
            self.assertEqual(sorted(p.name for p in (work / 'inference-release').iterdir()),
                             ['image-release.json', 'publisher.pem'])
            self.assertTrue(json.loads((work / 'case-result.json').read_text())['passed'])
            with patch.dict(os.environ, E2E_INFERENCE_FIXTURES=str(source.parent)):
                with self.assertRaisesRegex(RuntimeError, 'original local producer'):
                    inference_fixtures.debug_build(SimpleNamespace(suite=root))

    def test_debug_uses_the_original_owned_build_and_reverifies_it(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
            root = Path(directory)
            case = SimpleNamespace(suite=root)
            with patch.object(inference_fixtures, 'verify_resumable_build', return_value=True) as verify:
                service, spec = inference_fixtures.debug_build(case)
            self.assertEqual(case.state, root / 'prepare-inference-images/state')
            self.assertEqual(service, 'vllm-inference')
            verify.assert_called_once_with(case, service, spec, ['debug'])

    def test_operator_peering_is_created_in_the_prepared_debug_state(self):
        case = MagicMock(state=Path('/case-state'))
        case.__enter__.return_value = case
        case.__exit__.return_value = False
        class Prepared(Exception):
            pass
        def prepare_debug(selected):
            selected.state = Path('/original-build-state')
            return 'vllm-inference', Path('/original-spec')
        def operator():
            self.assertEqual(case.state, Path('/original-build-state'))
            raise Prepared()
        case.operator.side_effect = operator
        with patch.dict(os.environ, E2E_USE_PREPARED_DEBUG='1'), \
                patch.object(inference_case, 'Case', return_value=case), \
                patch.object(inference_fixtures, 'debug_build', side_effect=prepare_debug):
            with self.assertRaises(Prepared):
                inference_case.main()

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
