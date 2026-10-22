"""Validate scheduling and cleanup behavior without any cloud credentials."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from cloud_case import Case

spec = importlib.util.spec_from_file_location('suite', Path(__file__).resolve().parents[1] / 'suite.py')
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)


class SuiteTest(unittest.TestCase):
    def test_prebuilt_case_skips_cargo_but_still_requires_runtime_tools(self):
        writer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_complete':True}))"
        for prebuilt, required, expected in [
            ("1", ["cargo", "curl"], "passed"),
            ("0", ["cargo", "curl"], "blocked"),
            ("1", ["cargo", "required-runtime-tool"], "blocked"),
        ]:
            with self.subTest(prebuilt=prebuilt, required=required), tempfile.TemporaryDirectory() as directory:
                runner = suite.Suite(Path(directory), {}, dict(os.environ, E2E_SKIP_CARGO_BUILD=prebuilt), 1, 1)
                try:
                    with patch.object(suite.shutil, 'which', side_effect=lambda name, **_: '/test/curl' if name == 'curl' else None):
                        result = runner.execute('prebuilt', {
                            'command': [sys.executable, '-c', writer],
                            'required_commands': required,
                        })
                    self.assertEqual(result['status'], expected)
                    if expected == 'blocked':
                        missing = 'cargo' if prebuilt == '0' else 'required-runtime-tool'
                        self.assertIn(missing, result['reason'])
                finally:
                    for pool in runner.pools.values():
                        pool.shutdown()

    def test_standalone_prebuilt_checks_runtime_binary_without_requiring_cargo(self):
        prefix = '''
source "$ROOT_DIR/tools/e2e/lib/common.sh"
command() { if [[ "$1" == -v && ( "$2" == cargo || "$2" == required-runtime-tool ) ]]; then return 1; fi; builtin command "$@"; }
'''
        for prebuilt, body, expected in [
            ('1', 'require_cmd cargo', 0),
            ('0', 'require_cmd cargo', 2),
            ('1', 'require_cmd required-runtime-tool', 2),
            ('1', 'CA_BIN=/missing/candidate-ca; build_host_binaries', 2),
        ]:
            with self.subTest(prebuilt=prebuilt, body=body):
                result = subprocess.run(['bash', '-c', prefix + body],
                    env=dict(os.environ, ROOT_DIR=str(suite.ROOT), E2E_RUN_ID='prebuilt-test',
                             E2E_SKIP_CARGO_BUILD=prebuilt), capture_output=True)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_slow_producer_setup_gets_build_slots_before_unrelated_cases(self):
        worker = r"""
import json,os,sys,time
from pathlib import Path
sys.path.insert(0,sys.argv[1])
from command_runner import build_slot
work=Path(os.environ['E2E_WORK_DIR'])
name=os.environ['E2E_CASE_ID']
(work/'worker-started.json').write_text(json.dumps(time.monotonic()))
if name.startswith('producer'):
 time.sleep(0.2 if name=='producer-fast' else 0.6)
with build_slot(work/'build-slot.timing.json'):
 (work/'build-started.json').write_text(json.dumps(time.monotonic()))
 time.sleep(2.5 if name.startswith('producer') else 0.05)
(work/'build-finished.json').write_text(json.dumps(time.monotonic()))
(work/'case-result.json').write_text(json.dumps({'passed':True,'cleanup_complete':True}))
"""
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            command = [sys.executable, '-c', worker, str(suite.ROOT / 'tools/e2e')]
            cases = {name: {'command': command, 'retains_fixtures': name.startswith('producer')}
                     for name in ['ordinary', 'producer-fast', 'producer-slow']}
            environment = dict(os.environ, E2E_BUILD_SLOT_DIR=str(work / 'slots'), E2E_BUILD_JOBS='2')
            runner = suite.Suite(work, cases, environment, 4, 1)
            self.assertTrue(runner.run())
            read = lambda name, stage: json.loads((work/name/(stage+'.json')).read_text())
            producer_starts = [read(name, 'build-started') for name in cases if name.startswith('producer')]
            producer_finishes = [read(name, 'build-finished') for name in cases if name.startswith('producer')]
            self.assertGreaterEqual(read('ordinary', 'worker-started'), max(producer_starts))
            self.assertLess(read('ordinary', 'worker-started'), min(producer_finishes))
            self.assertGreaterEqual(read('ordinary', 'build-started'), min(producer_finishes))

    def test_cli_preserves_a_shared_build_budget_for_concurrent_suites(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shared = root / 'shared-build-slots'
            for name in ['first', 'second']:
                runner = Mock(results={}, run=Mock(return_value=True))
                environment = {'E2E_BUILD_SLOT_DIR': str(shared)}
                arguments = ['suite.py', '--suite', 'full', '--build-jobs', '2',
                             '--work-dir', str(root / name)]
                with patch.dict(os.environ, environment), patch.object(sys, 'argv', arguments), \
                        patch.object(suite, 'Suite', return_value=runner) as constructor, \
                        patch.object(suite.signal, 'signal'):
                    with self.assertRaises(SystemExit) as result:
                        suite.main()
                self.assertEqual(result.exception.code, 0)
                effective = constructor.call_args.args[2]
                self.assertEqual(effective['E2E_BUILD_SLOT_DIR'], str(shared))
                self.assertEqual(effective['E2E_BUILD_JOBS'], '2')
                metadata = json.loads((root / name / 'metadata.json').read_text())
                self.assertEqual(metadata['build_slot_dir'], str(shared))
                self.assertEqual(metadata['build_jobs'], 2)

    def test_termination_keeps_cleaning_up_when_a_case_exits_during_signal(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            runner = suite.Suite(Path(directory), {}, {}, 1, 1)
            runner.processes = {
                'just-exited': Mock(pid=101, poll=Mock(return_value=None)),
                'still-running': Mock(pid=202, poll=Mock(return_value=None)),
            }
            try:
                with patch.object(suite.os, 'killpg', side_effect=[ProcessLookupError(), None]) as kill:
                    runner.terminate()
                self.assertTrue(runner.stop.is_set())
                self.assertEqual([call.args[0] for call in kill.call_args_list], [101, 202])
            finally:
                for pool in runner.pools.values():
                    pool.shutdown()

    def test_case_retains_trustee_until_service_cleanup_retry_succeeds(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            service = work / 'state/services/example'
            service.mkdir(parents=True)
            (service / 'state.json').write_text('{"phase":"active"}')
            with patch.dict(os.environ, {'E2E_WORK_DIR': directory,
                                        'E2E_STATE_DIR': str(work / 'state')}):
                case = Case()
            case.resources = [{'state_dir': str(work / 'state'), 'service': 'example'}]
            case.trustee_work = work / 'trustee'
            with patch.object(case, 'record_resources'), patch.object(case, 'command') as command, \
                    patch.object(case, 'trustee_action') as trustee:
                command.side_effect = RuntimeError('provider still has dependencies')
                self.assertEqual(case.cleanup(), ['provider still has dependencies'])
                trustee.assert_not_called()
                command.side_effect = None
                self.assertEqual(case.cleanup(), [])
                trustee.assert_called_once_with('destroy')

    def test_cases_have_distinct_cloud_run_ids_even_with_shared_parent_id(self):
        writer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_complete':True,'run_id':os.environ.get('E2E_RUN_ID')}))"
        with tempfile.TemporaryDirectory() as directory:
            runner = suite.Suite(Path(directory), {}, dict(os.environ, E2E_RUN_ID='shared-parent'), 1, 1)
            try:
                with patch.object(suite.time, 'strftime', return_value='20261015112233'):
                    results = [runner.execute(name, {'command': [sys.executable, '-c', writer]})
                               for name in ['trustee-harness', 'trustee-inference']]
                self.assertTrue(all(result['status'] == 'passed' for result in results))
                ids = {result['run_id'] for result in results}
                self.assertEqual(len(ids), 2)
                self.assertNotIn('shared-parent', ids)
            finally:
                for pool in runner.pools.values():
                    pool.shutdown()

    def test_standalone_trustees_do_not_share_same_second_client_tokens(self):
        command = ['bash', '-c', '''
set -euo pipefail
date() { printf 20261015112233; }
source "$ROOT_DIR/tools/e2e/lib/common.sh"
source "$ROOT_DIR/tools/e2e/lib/trustee.sh"
TRUSTEE_NAME="cai-trustee-duo-$E2E_RUN_ID"
trustee_client_token vpc
''']
        environment = dict(os.environ, ROOT_DIR=str(suite.ROOT))
        environment.pop('E2E_RUN_ID', None)
        tokens = [subprocess.check_output(command, env=environment, text=True).strip() for _ in range(2)]
        self.assertNotEqual(*tokens)
        resumed = dict(environment, E2E_RUN_ID='explicit-resume-identity')
        tokens = [subprocess.check_output(command, env=resumed, text=True).strip() for _ in range(2)]
        self.assertEqual(*tokens)

    def test_gpu_lane_does_not_inherit_cpu_hardware(self):
        writer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_complete':True,'target':[os.environ['E2E_INSTANCE_TYPE'],os.environ['E2E_ZONE_ID']]}))"
        for gpu_type in [None, 'ecs.gn8v-tee.8xlarge']:
            with self.subTest(gpu_type=gpu_type), tempfile.TemporaryDirectory() as directory:
                environment = dict(os.environ, E2E_INSTANCE_TYPE='ecs.g9i.xlarge',
                                   E2E_ZONE_ID='cn-beijing-i', E2E_GPU_ZONE_ID='cn-beijing-l')
                environment.pop('E2E_GPU_INSTANCE_TYPE', None)
                if gpu_type:
                    environment['E2E_GPU_INSTANCE_TYPE'] = gpu_type
                runner = suite.Suite(Path(directory), {}, environment, 1, 1)
                try:
                    gpu = runner.execute('gpu', {'pool': 'gpu', 'command': [sys.executable, '-c', writer]})
                    cpu = runner.execute('cpu', {'pool': 'cpu', 'command': [sys.executable, '-c', writer]})
                    self.assertEqual(gpu['status'], 'passed')
                    self.assertEqual(gpu['target'], [gpu_type or 'ecs.gn8v-tee.4xlarge', 'cn-beijing-l'])
                    self.assertEqual(cpu['target'], ['ecs.g9i.xlarge', 'cn-beijing-i'])
                finally:
                    for pool in runner.pools.values():
                        pool.shutdown()

    def test_selection_preserves_producer_dependency_and_rejects_cycle(self):
        cases = {'image': {'groups': ['producer']},
                 'consumer': {'groups': ['inference'], 'depends': ['image']}}
        self.assertEqual(set(suite.select_cases(cases, ['inference'], [])), set(cases))
        cases['image']['depends'] = ['consumer']
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            suite.select_cases(cases, ['inference'], [])

    def test_failed_producer_does_not_start_its_consumer(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            writer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':False,'cleanup_complete':True}))"
            consumer = "import pathlib; pathlib.Path(" + repr(str(work / 'unexpected-consumer')) + ").touch()"
            cases = {'publish': {'retains_fixtures': True, 'command': [sys.executable, '-c', writer]},
                     'consumer': {'depends': ['publish'], 'command': [sys.executable, '-c', consumer]}}
            runner = suite.Suite(work, cases, dict(os.environ), 1, 1)
            self.assertFalse(runner.run())
            self.assertEqual(runner.results['consumer']['status'], 'blocked')
            self.assertEqual(json.loads((work / 'results.json').read_text())['consumer']['status'], 'blocked')
            self.assertFalse((work / 'unexpected-consumer').exists())

    def test_zero_exit_without_assertions_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = suite.Suite(Path(directory), {}, dict(os.environ), 1, 1)
            try:
                result = runner.execute('empty', {'command': [sys.executable, '-c', 'pass']})
                self.assertEqual(result['status'], 'failed')
            finally:
                for pool in runner.pools.values():
                    pool.shutdown()

    def test_trustee_cleanup_survives_missing_ca_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'trustee').mkdir()
            (work / 'trustee/trustee-infra-ids.json').write_text('{"instance_id":"i-owned"}')
            with (work / 'cleanup.log').open('wb') as log, patch.object(suite.subprocess, 'run') as run:
                run.return_value.returncode = 0
                self.assertTrue(suite.recover_cleanup(work, {}, log))
                self.assertEqual(run.call_args.args[0][-1], 'destroy')
                self.assertEqual(run.call_args.kwargs['env']['E2E_TRUSTEE_WORK_DIR'], str(work / 'trustee'))

    def test_legacy_trustee_cleanup_skips_already_deleted_service(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            service = work / 'state/services/example'
            service.mkdir(parents=True)
            suite.write_json(service / 'state.json', {'phase': 'deleted', 'attestation_mode': 'trustee'})
            suite.write_json(work / 'destroy-targets.json', [
                {'state_dir': str(work / 'state'), 'service': 'example'}])
            suite.write_json(work / 'trustee-infra-ids.json', {'instance_id': 'i-owned'})
            with (work / 'cleanup.log').open('wb') as log, patch.object(suite.subprocess, 'run') as run:
                run.return_value.returncode = 0
                self.assertTrue(suite.recover_cleanup(work, {'CA_BIN': '/installed/ca'}, log))
                self.assertEqual(run.call_count, 1)
                self.assertEqual(run.call_args.args[0][-2:], [str(suite.ROOT / 'tools/e2e/trustee_fixture.sh'), 'destroy'])
                self.assertEqual(run.call_args.kwargs['env']['E2E_TRUSTEE_WORK_DIR'], str(work))

    def test_failed_service_cleanup_retains_trustee_for_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            service = work / 'state/services/example'
            service.mkdir(parents=True)
            suite.write_json(service / 'state.json', {'phase': 'active', 'attestation_mode': 'trustee'})
            suite.write_json(work / 'destroy-targets.json', [
                {'state_dir': str(work / 'state'), 'service': 'example'}])
            (work / 'trustee').mkdir()
            suite.write_json(work / 'trustee/trustee-infra-ids.json', {'instance_id': 'i-owned'})
            with (work / 'cleanup.log').open('wb') as log, patch.object(suite.subprocess, 'run') as run:
                run.return_value.returncode = 1
                self.assertFalse(suite.recover_cleanup(work, {'CA_BIN': '/installed/ca'}, log))
                self.assertEqual(run.call_count, 1)
                self.assertEqual(run.call_args.args[0][-2:], ['destroy', 'example'])

    def test_registered_build_failure_has_no_infrastructure_to_destroy(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            suite.write_json(work / 'destroy-targets.json', [
                {'state_dir': str(work / 'state'), 'service': 'interrupted-build'}])
            with (work / 'cleanup.log').open('wb') as log, patch.object(suite.subprocess, 'run') as run:
                self.assertTrue(suite.recover_cleanup(work, {}, log))
                run.assert_not_called()

    def test_only_unpublished_local_builds_use_image_cleanup(self):
        for change, local_only in [({}, True), ({'phase': 'deploying'}, False),
                                   ({'deploy': {'terraform_dir': '/saved/attempt'}}, False),
                                   ({'deploy': {'instance_ids': ['i-owned']}}, False),
                                   ({'build': {'published': {'release': {'image_id': 'm-owned'}}}}, False)]:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                state_dir = work / 'state'
                service = state_dir / 'services/example'
                service.mkdir(parents=True)
                state = {'phase': 'built', 'build': {}, 'deploy': {
                    'run_id': 'planned', 'resource_name': 'planned', 'image_import_name': 'planned', 'tee': 'tdx'}}
                state.update(change)
                suite.write_json(service / 'state.json', state)
                suite.write_json(work / 'destroy-targets.json', [
                    {'state_dir': str(state_dir), 'service': 'example'}])
                with (work / 'cleanup.log').open('wb') as log, patch.object(suite.subprocess, 'run') as run:
                    run.return_value.returncode = 0
                    self.assertTrue(suite.recover_cleanup(work, {'CA_BIN': '/installed/ca'}, log))
                    operation = ['image', 'rm', 'example', '--force'] if local_only else ['destroy', 'example']
                    self.assertEqual(run.call_args.args[0], ['/installed/ca', '--state-dir', str(state_dir), *operation])
                    run.return_value.returncode = 1
                    self.assertFalse(suite.recover_cleanup(work, {'CA_BIN': '/installed/ca'}, log))
                    if local_only:
                        (state_dir / 'trustee').mkdir()
                        suite.write_json(state_dir / 'trustee/state.json', {'services': {'example': {}}})
                        run.return_value.returncode = 0
                        self.assertTrue(suite.recover_cleanup(work, {'CA_BIN': '/installed/ca'}, log))
                        self.assertEqual(run.call_args.args[0][-2:], ['destroy', 'example'])

    def test_interrupted_queued_case_never_starts(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = suite.Suite(Path(directory), {}, dict(os.environ), 1, 1)
            runner.stop.set()
            try:
                with patch.object(suite.subprocess, 'Popen') as process:
                    result = runner.execute('queued', {'command': ['unexpected']})
                    self.assertEqual(result['status'], 'blocked')
                    process.assert_not_called()
            finally:
                for pool in runner.pools.values():
                    pool.shutdown()

    def test_active_independent_consumer_prevents_image_unpublish(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            lease = work / 'independent-consumer.lease'
            lease.touch()
            writer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_deferred':True}))"
            cases = {name: {'retains_fixtures': True, 'command': [sys.executable, '-c', writer]}
                     for name in ['publish-reuse-images', 'prepare-inference-images']}
            environment = dict(os.environ, E2E_FIXTURE_CONSUMER_LEASE=str(lease),
                               E2E_FIXTURE_LEASE_TIMEOUT_SEC='0')
            runner = suite.Suite(work, cases, environment, 1, 1)
            with patch.object(suite.subprocess, 'run') as cleanup:
                self.assertFalse(runner.run())
                cleanup.assert_not_called()
            self.assertTrue(lease.exists())
            self.assertFalse(runner.results['publish-reuse-images']['cleanup_complete'])
            self.assertFalse(runner.results['prepare-inference-images']['cleanup_complete'])

    def test_consumer_cleanup_failure_retains_only_its_shared_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            producer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_deferred':True}))"
            consumer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':False,'cleanup_complete':False}))"
            cases = {name: {'retains_fixtures': True, 'command': [sys.executable, '-c', producer],
                            'cleanup_command': ['fake-cleanup', name]} for name in ['images', 'unrelated']}
            cases['consumer'] = {'depends': ['images'], 'command': [sys.executable, '-c', consumer]}
            runner = suite.Suite(work, cases, dict(os.environ), 2, 1)
            with patch.object(suite, 'recover_cleanup', return_value=False), \
                    patch.object(suite.subprocess, 'run') as cleanup:
                cleanup.return_value.returncode = 0
                self.assertFalse(runner.run())
            self.assertEqual(cleanup.call_count, 1)
            self.assertEqual(cleanup.call_args.args[0], ['fake-cleanup', 'unrelated'])
            self.assertFalse(runner.results['images']['cleanup_complete'])

    def test_fixture_cleanup_continues_after_another_producer_cleanup_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            producer = "import os,json,pathlib; pathlib.Path(os.environ['E2E_WORK_DIR'],'case-result.json').write_text(json.dumps({'passed':True,'cleanup_deferred':True}))"
            cases = {name: {'retains_fixtures': True, 'command': [sys.executable, '-c', producer],
                            'cleanup_command': ['fake-cleanup', name]} for name in ['first', 'second']}
            runner = suite.Suite(Path(directory), cases, dict(os.environ), 1, 1)
            with patch.object(suite.subprocess, 'run') as cleanup:
                from types import SimpleNamespace
                cleanup.side_effect = [SimpleNamespace(returncode=1), SimpleNamespace(returncode=0)]
                self.assertFalse(runner.run())
            self.assertEqual(cleanup.call_count, 2)
            self.assertEqual(sum(r['cleanup_complete'] for r in runner.results.values()), 1)

    def test_missing_host_command_blocks_before_case_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = suite.Suite(Path(directory), {}, dict(os.environ), 1, 1)
            try:
                with patch.object(suite.subprocess, 'Popen') as process:
                    result = runner.execute('prerequisite', {
                        'required_commands': ['ca-e2e-command-that-does-not-exist'],
                        'command': ['unexpected']})
                    self.assertEqual(result['status'], 'blocked')
                    self.assertTrue(result['cleanup_complete'])
                    process.assert_not_called()
            finally:
                for pool in runner.pools.values():
                    pool.shutdown()


if __name__ == '__main__':
    unittest.main()
