"""Encrypted reboot proof must reject wrong guests and incomplete boot evidence."""
import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/e2e'))
SPEC = importlib.util.spec_from_file_location('cryptpilot_reboot', ROOT / 'tools/e2e/allinone-reuse.py')
REUSE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REUSE)
CONSOLE = b'Resizing ext4 filesystem to fill device device=/dev/mapper/rootfs\next4 filesystem resized successfully\n'


def filesystems():
    geometry = {'mount': {'fstype': 'ext4'}, 'device_bytes': 4096 * 100,
                'filesystem_blocks': 100, 'filesystem_block_bytes': 4096,
                'filesystem_state': 'clean', 'statvfs_bytes': 4096 * 90,
                'cryptpilot_root_device': True,
                'device_chain': [{'type': 'dm', 'children': [{'type': 'crypt'}]}]}
    return {'boot_id': 'boot-before', 'commands': {
        'crypt': {'stdout': 'delta: 0 100 crypt', 'exit_code': 0},
        'verity': {'stdout': 'root: 0 100 verity', 'exit_code': 0}},
        'mounts': {'root': copy.deepcopy(geometry), 'data': copy.deepcopy(geometry)}}


class CryptpilotBootTest(unittest.TestCase):
    def test_resize_success_requires_original_console_and_healthy_bound_filesystems(self):
        REUSE.verify_boot_storage(filesystems(), CONSOLE, b'', True)
        REUSE.verify_boot_storage(filesystems(), CONSOLE, b'EXT4-fs (dm-0): mounted filesystem. Opts: errors=remount-ro', True)
        for console in [b'', b'ext4 filesystem resized successfully',
                        b'ext4 filesystem resized successfully\nResizing ext4 filesystem to fill device device=/dev/mapper/rootfs\n',
                        CONSOLE.replace(b'/dev/mapper/rootfs', b'/dev/mapper/unrelated')]:
            with self.subTest(console=console), self.assertRaises(AssertionError):
                REUSE.verify_boot_storage(filesystems(), console, CONSOLE, True)
        for change in ['plain-data', 'small-root', 'wrong-root', 'command-failed']:
            evidence = filesystems()
            if change == 'plain-data':
                evidence['mounts']['data']['device_chain'] = [{'type': 'part'}]
            elif change == 'small-root':
                evidence['mounts']['root']['filesystem_blocks'] = 80
            elif change == 'wrong-root':
                evidence['mounts']['root']['cryptpilot_root_device'] = False
            else:
                evidence['commands']['crypt']['exit_code'] = 1
            with self.subTest(change=change), self.assertRaises(AssertionError):
                REUSE.verify_boot_storage(evidence, CONSOLE, b'', True)

    def test_resize_success_never_hides_integrity_io_or_filesystem_failure(self):
        for failure in [b'AEAD: authentication failed', b'Buffer I/O error on dev dm-0',
                        b'EXT4-fs error (device dm-0)', b'Filesystem check failed with exit code 4',
                        b'Failed to resize ext4 filesystem on /dev/mapper/rootfs', b'Failed to mount /sysroot']:
            for source in ['console', 'journal']:
                with self.subTest(failure=failure, source=source), self.assertRaises(AssertionError):
                    REUSE.verify_boot_storage(filesystems(), CONSOLE + (failure if source == 'console' else b''),
                                              failure if source == 'journal' else b'', True)

    def test_real_guest_digest_and_exact_first_version_line_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            for digest, version in [(REUSE.CRYPTPILOT_GUEST_SHA256, 'cryptpilot-fde 0.9.2\n'),
                                    ('0' * 64, 'cryptpilot-fde 0.9.3\n')]:
                def ssh(label, *_):
                    value = digest + '  /usr/bin/cryptpilot-fde\n' if label.endswith('sha256') else version
                    return SimpleNamespace(stdout=value.encode())
                consumer = SimpleNamespace(work=Path(directory), ssh=Mock(side_effect=ssh))
                with self.subTest(digest=digest, version=version), self.assertRaises(AssertionError):
                    REUSE.cryptpilot_boot_evidence(consumer, 'first-boot', b'boot-before', CONSOLE, True)
                self.assertLessEqual(consumer.ssh.call_count, 2)

    def test_console_keeps_raw_response_identity_and_state_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            state = b'{"deploy":{"instance_id":"i-owned"}}'
            (work / 'state.json').write_bytes(state)
            consumer = SimpleNamespace(directory=work, work=work, spec={'deploy': {'region': 'cn-beijing'}})
            for instance in ['i-owned', 'i-other']:
                raw = json.dumps({'InstanceId': instance, 'LastUpdateTime': 'observed',
                                  'ConsoleOutput': base64.b64encode(CONSOLE).decode()}).encode()
                with patch.object(REUSE, 'run', return_value=SimpleNamespace(stdout=raw)) as run:
                    if instance != 'i-owned':
                        with self.assertRaises(AssertionError):
                            REUSE.capture_boot_console(consumer, 'aliyun', 'first-console', b'boot-before')
                        continue
                    self.assertEqual(REUSE.capture_boot_console(consumer, 'aliyun', 'first-console', b'boot-before'), CONSOLE)
                receipt = json.loads((work / 'first-console-receipt.json').read_text())
                self.assertEqual(receipt['state_sha256'], hashlib.sha256(state).hexdigest())
                self.assertEqual(receipt['response_sha256'], hashlib.sha256(raw).hexdigest())
                self.assertEqual((work / 'first-console-state.private.json').read_bytes(), state)
                self.assertIn('--RemoveSymbols', run.call_args.args[0])

    def test_reboot_retains_force_stop_changed_boot_and_fresh_marker_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'state.json').write_text(json.dumps({'deploy': {'instance_id': 'i-owned', 'public_ip': '203.0.113.1'}}))
            def ssh(label, *_args, **_kwargs):
                return SimpleNamespace(returncode=0, stdout=b'boot-before\n' if label == 'boot-before' else b'boot-after\n')
            consumer = SimpleNamespace(directory=work, work=work, service='owned',
                spec={'deploy': {'region': 'cn-beijing'}, 'attestation': {'mode': 'trustee'}},
                ssh=Mock(side_effect=ssh), directory_ports=Mock(return_value={'peer': [1]}))
            with patch.object(REUSE, 'run') as run, patch.object(REUSE.time, 'sleep'), \
                    patch.object(REUSE, 'capture_boot_console', return_value=CONSOLE), \
                    patch.object(REUSE, 'cryptpilot_boot_evidence') as proof:
                REUSE.reboot(consumer, 'aliyun')
            self.assertIn('--ForceStop', run.call_args.args[0])
            self.assertEqual(run.call_args.args[0][-1], 'true')
            written = next(call.args[1] for call in consumer.ssh.call_args_list if call.args[0] == 'persistent-write')
            marker = re.search(r'\b[a-f0-9]{64}\b', written).group()
            checked = next(call.args[1] for call in consumer.ssh.call_args_list if call.args[0] == 'persistent-after')
            self.assertIn(marker, checked)
            for command in [written, checked]:
                result = subprocess.run(['bash', '-c', command.replace('/opt/data/allinone/reboot-marker', str(work / 'marker'))],
                                        capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((work / 'marker').read_text(), marker + '\n')
            self.assertEqual(proof.call_count, 2)
            evidence = json.loads((work / 'encrypted-reboot-proof.json').read_text())
            self.assertEqual(evidence['persistent_marker_sha256'], hashlib.sha256((work / 'marker').read_bytes()).hexdigest())
            self.assertNotEqual(evidence['boot_before'], evidence['boot_after'])
            self.assertTrue(evidence['force_stop'])


if __name__ == '__main__':
    unittest.main()
