"""Negative app delivery must prove management health without claiming app readiness."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

E2E = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(E2E))
spec = importlib.util.spec_from_file_location('negative_reuse', E2E / 'allinone-reuse.py')
reuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reuse)


class NegativeApplicationInjectionTest(unittest.TestCase):
    def make(self, directory, code=1, change=None, missing=False, error=None):
        root = Path(directory)
        consumer = object.__new__(reuse.Consumer)
        consumer.work = root
        consumer.state = root / 'state'
        consumer.service = 'test-service'
        consumer.spec_path = root / 'appspec.json'
        consumer.directory.mkdir(parents=True)
        cloud = {'public_ip': '192.0.2.1', 'instance_id': 'i-owned'}
        node = {'address': cloud['public_ip'], 'instance_id': cloud['instance_id'],
                'live_error': None, 'daemon': {'service_id': consumer.service,
                'mesh_ready': True, 'app_ready': False}}
        if change:
            change(node)
        if not missing:
            (consumer.directory / 'replica-readiness.json').write_text(json.dumps([node]))
        err = ("Error: not all replicas of 'test-service' became ready within 300s; "
               "inspect replica-readiness.json and resume delivery with the committed inputs\n").encode()
        consumer.command = Mock(return_value=subprocess.CompletedProcess([], code, b'', err if error is None else error))
        return consumer, cloud

    def test_only_app_unready_is_accepted_and_original_readiness_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            c, cloud = self.make(directory)
            c.inject_application_error('invalid', cloud)
            source = c.directory / 'replica-readiness.json'
            snapshot = c.work / 'invalid-replica-readiness.json'
            original = source.read_bytes()
            source.write_text('[]')
            self.assertEqual(snapshot.read_bytes(), original)
            c.command.assert_called_once_with('invalid', 'inject', '--spec', str(c.spec_path),
                '--target-ip', cloud['public_ip'], timeout=600, success=False)

    def test_other_failure_causes_and_targets_are_rejected(self):
        changes = [lambda n: n['daemon'].update(mesh_ready=False),
                   lambda n: n['daemon'].update(app_ready=True),
                   lambda n: n['daemon'].update(service_id='other'),
                   lambda n: n.update(live_error='connection reset'),
                   lambda n: n.update(instance_id='i-other'),
                   lambda n: n.update(address='192.0.2.2'),
                   lambda n: n.update(daemon=None)]
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                c, cloud = self.make(directory, change=change)
                with self.assertRaises(AssertionError):
                    c.inject_application_error('invalid', cloud)

    def test_unrelated_cli_errors_and_timeout_never_accepted(self):
        for code, err in [(124, None), (2, None), (1, b'Error: authentication failed\n'),
                          (1, b'Error: delivery failed\nError: unrelated\n')]:
            with self.subTest(code=code, error=err), tempfile.TemporaryDirectory() as directory:
                c, cloud = self.make(directory, code=code, error=err)
                with self.assertRaises(AssertionError):
                    c.inject_application_error('invalid', cloud)

    def test_missing_empty_duplicate_or_malformed_readiness_fails(self):
        for value in [None, b'[]', b'[{},{}]', b'not-json']:
            with self.subTest(value=value), tempfile.TemporaryDirectory() as directory:
                c, cloud = self.make(directory, missing=value is None)
                if value is not None:
                    (c.directory / 'replica-readiness.json').write_bytes(value)
                with self.assertRaises((AssertionError, ValueError)):
                    c.inject_application_error('invalid', cloud)

    def test_success_can_proceed_to_separate_delivery_and_negative_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            c, cloud = self.make(directory, code=0, missing=True, error=b'')
            self.assertEqual(c.inject_application_error('invalid', cloud).returncode, 0)

    def test_exact_delivered_bad_bytes_are_required_and_restore_is_strict(self):
        for stale in [False, True]:
            with self.subTest(stale=stale), tempfile.TemporaryDirectory() as directory:
                c, cloud = self.make(directory, code=0)
                source = Path(directory) / 'runtime.json'
                guest = Path(directory) / 'guest-runtime.json'
                original = b'{"healthy":true}'
                source.write_bytes(original)
                guest.write_bytes(original)
                (c.directory / 'state.json').write_text(json.dumps({'deploy': cloud}))
                c.spec = {'resources': {'runtime': {'source': str(source),
                    'target': '/opt/data/allinone/runtime.json'}}}
                calls = []
                def command(label, *args, **kwargs):
                    calls.append((label, kwargs))
                    if not stale or label == 'restore-application':
                        guest.write_bytes(source.read_bytes())
                    return subprocess.CompletedProcess([], 0, b'', b'')
                c.command = command
                def ssh(label, script, **kwargs):
                    if label.endswith('-check'):
                        script = script.replace('/opt/data/allinone/runtime.json', str(guest))
                        script = script.replace('cai-agent --check',
                            "printf 'allinone startup failed: deliberate error' >&2; exit 1")
                        return subprocess.run(['bash', '-c', script], capture_output=True)
                    return subprocess.CompletedProcess([], 0, b'', b'')
                c.ssh = ssh
                with patch.object(reuse.time, 'monotonic', side_effect=[0, 100] if stale else [0, 0]):
                    if stale:
                        with self.assertRaisesRegex(AssertionError, 'not delivered'):
                            c.verify_management_during_application_errors()
                    else:
                        c.verify_management_during_application_errors()
                self.assertEqual(source.read_bytes(), original)
                self.assertEqual(guest.read_bytes(), original)
                self.assertEqual(calls[-1][0], 'restore-application')
                self.assertNotIn('success', calls[-1][1])

    def test_existing_evidence_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            c, cloud = self.make(directory)
            path = c.work / 'invalid-replica-readiness.json'
            path.write_bytes(b'first')
            with self.assertRaises(FileExistsError):
                c.inject_application_error('invalid', cloud)
            self.assertEqual(path.read_bytes(), b'first')


if __name__ == '__main__':
    unittest.main()
