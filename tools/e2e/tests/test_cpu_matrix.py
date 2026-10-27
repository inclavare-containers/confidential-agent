"""CPU lane consolidation must retain independent failure and model probes."""
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

E2E = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(E2E))
from fixtures.allinone import generate
from platform_case import add_platform_fixtures
import publish_fixtures

spec = importlib.util.spec_from_file_location("cpu_matrix_reuse", E2E / "allinone-reuse.py")
reuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reuse)


class CpuMatrixTests(unittest.TestCase):
    def test_route_churn_preserves_six_native_model_probes_and_private_extra(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'DASHSCOPE_API_KEY': 'private-bailian', 'QODER_PERSONAL_ACCESS_TOKEN': 'private-qoder',
                'E2E_QODER_MODEL': 'fixed-test-model'}):
            root = Path(directory)
            selected = generate(root / 'fixtures', {'release': '/signed-release'}, 'challenge', 'test-region', 'allinone')
            original = {p.name: json.loads((p / 'probe.json').read_text())['command'] for p in selected}
            selected, extra = add_platform_fixtures(root, selected, 'allinone')
            self.assertEqual(len(selected), 6)
            self.assertEqual({p.name: json.loads((p / 'probe.json').read_text())['command'] for p in selected}, original)
            consumers = [SimpleNamespace(service=json.loads((p / 'appspec.json').read_text())['service']['id'],
                         spec=json.loads((p / 'appspec.json').read_text())) for p in selected]
            peers = reuse.stable_endpoint_peers(consumers, json.loads((extra / 'appspec.json').read_text()))
            self.assertEqual({p.service for p in peers}, {'allinone-openclaw', 'allinone-hermes'})
            self.assertEqual(json.loads((next(p for p in selected if p.name == 'openclaw') / 'runtime.json').read_text())['port'], 8642)
            self.assertTrue(all(original.values()))
            self.assertEqual((extra / 'runtime.json').stat().st_mode & 0o077, 0)

    def test_concurrency_and_resume_execute_on_different_consumers(self):
        consumers = [SimpleNamespace(deploy=Mock()) for _ in range(3)]
        args = SimpleNamespace(interrupt_infrastructure=True, aliyun='aliyun', frozen_inputs=False,
                               concurrent_deploy=True, resume_deploy=True, parallel=2)
        with patch.object(reuse, 'interrupt_infrastructure') as interrupt, \
                patch.object(reuse, 'assert_one_deployment') as unique:
            reuse.deploy_consumers(consumers, args)
        interrupt.assert_called_once_with(consumers[0], 'aliyun')
        unique.assert_called_once_with(consumers[0], 'aliyun')
        consumers[0].deploy.assert_called_once_with(concurrent=True, resume=False)
        consumers[1].deploy.assert_called_once_with(resume=True)
        consumers[2].deploy.assert_called_once_with(resume=False)
        args.frozen_inputs = True
        with patch.object(reuse, 'interrupt_infrastructure'), patch.object(reuse, 'assert_one_deployment'), \
                patch.object(reuse, 'deploy_with_frozen_inputs') as frozen:
            reuse.deploy_consumers(consumers, args)
        frozen.assert_called_once_with(consumers, 2, 'aliyun', resume=True)

    def test_no_ssh_smoke_has_a_real_protected_model_probe(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, DASHSCOPE_API_KEY='private'):
            root = Path(directory)
            fixture, = generate(root / 'strict', {}, 'challenge', 'test-region', 'strict', harnesses=['hermes'], ssh=False)
            spec = json.loads((fixture / 'appspec.json').read_text())
            probe = json.loads((fixture / 'probe.json').read_text())
            self.assertFalse(spec['service']['ssh'])
            self.assertNotIn('build', spec)
            self.assertEqual(spec['service']['connect'], [{'port': 8642, 'encryption': 'ohttp'}])
            self.assertIsNone(probe['command'])
            self.assertTrue(probe['http_model']['model'])
            runtime = json.loads((fixture / 'runtime.json').read_text())
            self.assertEqual(runtime['environment']['API_SERVER_KEY'], probe['http_model']['token'])
            with self.assertRaisesRegex(ValueError, 'protected Hermes'):
                generate(root / 'wrong', {}, 'challenge', 'test-region', 'bad', harnesses=['codex'], ssh=False)

    def test_full_hardening_cannot_be_claimed_for_partial_or_ssh_builds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            service = root / 'state/services/allinone-hardened-builder'
            service.mkdir(parents=True)
            rendered = service / 'shelter.yaml'
            manifest = {'shelter_config': str(rendered), 'runtime_contract': {
                'image_variant': 'release', 'capabilities': ['resources-v1']}}
            case = SimpleNamespace(state=root / 'state', checks={})
            for mode, capability, ssh in [('partial', None, None), ('full', 'instance-ssh-v1', None),
                                           ('full', None, {'private_key': '/producer-key'})]:
                with self.subTest(mode=mode, capability=capability, ssh=ssh):
                    rendered.write_text('security:\n  harden:\n    enabled: true\n    mode: ' + mode + '\n')
                    changed = json.loads(json.dumps(manifest))
                    changed['ssh'] = ssh
                    if capability:
                        changed['runtime_contract']['capabilities'].append(capability)
                    (service / 'manifest.json').write_text(json.dumps(changed))
                    with self.assertRaises(AssertionError):
                        publish_fixtures.verify_full_hardening_build(case)
                    self.assertFalse(case.checks)
            (service / 'manifest.json').write_text(json.dumps(manifest))
            publish_fixtures.verify_full_hardening_build(case)
            self.assertTrue(case.checks['full_hardening_release_built'])

    def test_outage_destroy_refusal_precedes_reboot_and_restores_trustee(self):
        for status, mutate in [(0, False), (124, False), (1, True), (1, False)]:
            with self.subTest(status=status, mutate=mutate), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                service = root / 'state/services/one'
                service.mkdir(parents=True)
                trustee = root / 'state/trustee/state.json'
                trustee.parent.mkdir(parents=True)
                trustee.write_text('original policy custody')
                cloud = {'instance_id': 'i-one'}
                (service / 'state.json').write_text(json.dumps({'phase': 'active', 'deploy': cloud}))
                def command(*args, **kwargs):
                    if mutate:
                        trustee.write_text('lost policy custody')
                    return subprocess.CompletedProcess([], status, b'', b'failed to reach Trustee')
                def ssh(label, *args, **kwargs):
                    return subprocess.CompletedProcess([], 1 if label.startswith('outage-ssh-') else 0,
                        b'new-boot' if label == 'outage-boot-after' else b'old-boot', b'')
                consumer = SimpleNamespace(work=root, state=root / 'state', directory=service,
                    spec={'deploy': {'region': 'test-region'}}, service='one', command=command, ssh=ssh)
                peer = SimpleNamespace(ssh=Mock())
                calls = []
                def run(args, *unused, **options):
                    calls.append(args)
                    data = {'Instances': {'Instance': [{'Status': 'Stopped'}]}}
                    if 'GetInstanceConsoleOutput' in args:
                        data = {'ConsoleOutput': base64.b64encode(b'Trustee key release failed').decode()}
                    return subprocess.CompletedProcess([], 0, json.dumps(data).encode(), b'')
                with patch.object(reuse, 'run', side_effect=run), patch.object(reuse.time, 'sleep'), \
                        patch.object(reuse, 'assert_one_deployment'):
                    if status in [0, 124] or mutate:
                        with self.assertRaises(AssertionError):
                            reuse.trustee_outage(consumer, 'aliyun', control='/owned-trustee', peers=[peer])
                        self.assertFalse(any('RebootInstance' in args for args in calls))
                    else:
                        reuse.trustee_outage(consumer, 'aliyun', control='/owned-trustee', peers=[peer])
                        peer.ssh.assert_called_once()
                self.assertIn(['/owned-trustee', 'start'], calls)

    def test_policy_lifecycle_requires_joint_current_guest_readiness(self):
        consumers = [SimpleNamespace(service=name, attempted=True) for name in ['one', 'two']]
        ready = [{'local': {'service_id': item.service, 'phase': 'active', 'mesh_generation': 4},
                  'daemon': {'phase': 'running', 'app_ready': True, 'mesh_ready': True,
                             'mesh_generation': 4}, 'live_error': None} for item in consumers]
        self.assertEqual(reuse.assert_live_lifecycle(ready, consumers), {'one': 4, 'two': 4})
        for change in [lambda row: row['daemon'].update(mesh_generation=3),
                       lambda row: row['daemon'].update(mesh_ready=False),
                       lambda row: row.update(live_error='unreachable'),
                       lambda row: row['local'].update(phase='deployed')]:
            rows = copy.deepcopy(ready)
            change(rows[0])
            with self.assertRaises(AssertionError):
                reuse.assert_live_lifecycle(rows, consumers)
        with self.assertRaises(AssertionError):
            reuse.assert_live_lifecycle([ready[0], ready[0]], consumers)
        consumers[0].attempted = False
        with self.assertRaises(AssertionError):
            reuse.assert_live_lifecycle(ready, consumers)
        ready[0]['local']['phase'] = 'deleted'
        ready[0]['daemon'] = None
        self.assertEqual(reuse.assert_live_lifecycle(ready, consumers), {'one': 4, 'two': 4})

    @unittest.skipUnless(os.environ.get('CA_BIN'), 'requires the candidate native CLI schema')
    def test_strict_consumer_validates_with_native_schema(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, DASHSCOPE_API_KEY='fixture'):
            root = Path(directory)
            release, key = root / 'release.json', root / 'public.pem'
            release.write_text('{}')
            key.write_text('schema fixture')
            fixture, = generate(root / 'fixtures', {'release': str(release), 'trusted_key': str(key),
                'manifest': 'sha256:' + 'a' * 64}, 'challenge', 'cn-beijing', 'strict', harnesses=['hermes'], ssh=False)
            result = subprocess.run([os.environ['CA_BIN'], 'spec', 'validate', '--spec', str(fixture / 'appspec.json')],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
