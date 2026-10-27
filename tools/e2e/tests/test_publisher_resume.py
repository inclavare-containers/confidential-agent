import hashlib
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import publish_fixtures


class PublisherResumeTest(unittest.TestCase):
    def test_resume_keeps_original_generated_or_legacy_specification(self):
        with tempfile.TemporaryDirectory() as directory:
            case = SimpleNamespace(work=Path(directory), command=Mock())
            legacy = case.work / 'examples/allinone/build.yaml'
            legacy.parent.mkdir(parents=True)
            legacy.write_text('original legacy specification')
            self.assertEqual(publish_fixtures.allinone_spec(case, case.work / 'cosign.key', True), legacy)
            generated = case.work / 'init/allinone-builder/build.yaml'
            generated.parent.mkdir(parents=True)
            generated.write_text('original generated specification')
            self.assertEqual(publish_fixtures.allinone_spec(case, case.work / 'cosign.key', True), generated)
            case.command.assert_not_called()
            self.assertEqual(legacy.read_text(), 'original legacy specification')
            self.assertEqual(generated.read_text(), 'original generated specification')
            strict = case.work / 'init/allinone-hardened-builder/build.yaml'
            strict.parent.mkdir(parents=True)
            strict.write_text('original strict specification')
            self.assertEqual(publish_fixtures.allinone_spec(case, case.work / 'cosign.key', True,
                                                          hardened=True), strict)
            self.assertEqual(strict.read_text(), 'original strict specification')
            case.command.assert_not_called()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.directory = root / 'state/services/builder'
        self.directory.mkdir(parents=True)
        self.spec = root / 'original.yaml'
        self.spec.write_text('original build specification\n')
        self.state = {'service_id': 'builder', 'phase': 'built', 'deploy': {},
                      'spec': {'path': str(self.spec), 'sha256': hashlib.sha256(self.spec.read_bytes()).hexdigest()}}
        self.manifest = {'service_id': 'builder', 'shelter_work_dir': str(self.directory / 'shelter'), 'variants': {}}
        for variant in ['release', 'debug']:
            directory = self.directory / variant
            directory.mkdir()
            (directory / 'artifact-manifest.json').write_text('{}')
            self.manifest['variants'][variant] = {'build_result': str(directory / 'build-result.json')}
        self.case = SimpleNamespace(state=root / 'state', work=root, checks={},
                                    local_state=lambda service: json.loads((self.directory / 'state.json').read_text()))
        self.save()

    def save(self):
        (self.directory / 'state.json').write_text(json.dumps(self.state))
        (self.directory / 'manifest.json').write_text(json.dumps(self.manifest))

    def verify(self):
        return publish_fixtures.verify_resumable_build(self.case, 'builder', self.spec, ['release', 'debug'])

    def test_changed_specification_or_deployment_cannot_reuse_build(self):
        original = self.spec.read_bytes()
        self.spec.write_text('changed build specification\n')
        with patch.object(publish_fixtures, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'original specification'):
                self.verify()
            run.assert_not_called()
        self.spec.write_bytes(original)
        for change in [{'phase': 'active'}, {'deploy': {'instance_ids': ['i-existing']}}]:
            with self.subTest(change=change), patch.object(publish_fixtures, 'run') as run:
                self.state.update(change)
                self.save()
                with self.assertRaises(RuntimeError):
                    self.verify()
                run.assert_not_called()
                self.state.update(phase='built', deploy={})

    def test_missing_variant_or_foreign_artifact_is_rejected_before_verification(self):
        original = dict(self.manifest['variants'])
        del self.manifest['variants']['debug']
        self.save()
        with patch.object(publish_fixtures, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'missing a completed build variant'):
                self.verify()
            run.assert_not_called()
        self.manifest['variants'] = original
        self.manifest['variants']['debug']['build_result'] = str(self.case.work / 'build-result.json')
        (self.case.work / 'artifact-manifest.json').write_text('{}')
        self.save()
        with patch.object(publish_fixtures, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'outside its owned state'):
                self.verify()
            run.assert_not_called()

    def test_failed_native_verification_cannot_be_replaced_by_recorded_success(self):
        with patch.object(publish_fixtures, 'run', side_effect=[None, RuntimeError('artifact verification failed')]):
            with self.assertRaisesRegex(RuntimeError, 'artifact verification failed'):
                self.verify()
        self.assertNotIn('resumed_builds_verified', self.case.checks)
        with patch.object(publish_fixtures, 'run') as run:
            self.assertTrue(self.verify())
            self.assertEqual(run.call_count, 2)
            self.assertTrue(all(call.args[0][3:5] == ['image', 'verify'] for call in run.call_args_list))
        self.assertEqual(self.case.checks['resumed_builds_verified'], ['builder'])

    def test_unfinished_service_still_requires_its_normal_build(self):
        (self.directory / 'state.json').unlink()
        with patch.object(publish_fixtures, 'run') as run:
            self.assertFalse(self.verify())
            run.assert_not_called()

    def test_cleaned_deployment_reverifies_retained_local_build_before_reuse(self):
        self.state['phase'] = 'deleted'
        self.save()
        with patch.object(publish_fixtures, 'run') as run:
            self.assertTrue(self.verify())
            self.assertEqual(run.call_count, 2)
        self.state['deploy'] = {'instance_ids': ['i-not-cleaned']}
        self.save()
        with patch.object(publish_fixtures, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'deployed service'):
                self.verify()
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
