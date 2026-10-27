"""Replay curl transport failures without GPU software or network transfers."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = [ROOT / 'examples/vllm-inference/install-vllm-inference.sh',
           ROOT / 'examples/openclaw-vllm/install-openclaw-vllm.sh']


class InstallerTransportRetries(unittest.TestCase):
    def invoke(self, root, script, outcomes):
        function = re.search(r'(?ms)^download_installer\(\) \{\n.*?^\}\n', script.read_text())
        self.assertIsNotNone(function)
        binaries = root / 'bin'
        binaries.mkdir()
        curl = binaries / 'curl'
        curl.write_text('''#!/usr/bin/env python3.11
import json,os,sys
from pathlib import Path
args=sys.argv[1:]
record=Path(os.environ['TEST_RECORD'])
calls=json.loads(record.read_text()) if record.exists() else []
outcomes=json.loads(os.environ['TEST_OUTCOMES'])
outcome=outcomes[min(len(calls),len(outcomes)-1)]
target=Path(args[args.index('--output')+1])
calls.append({'args':args,'temporary_existed':target.exists()})
record.write_text(json.dumps(calls))
target.write_text('complete installer' if outcome[0]==0 else 'partial installer')
print(outcome[1],end='')
raise SystemExit(outcome[0])
''')
        curl.chmod(0o755)
        sleeper = binaries / 'sleep'
        sleeper.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$TEST_SLEEPS"\n')
        sleeper.chmod(0o755)
        destination = root / 'installer.run'
        # A previous interrupted download must not become the next attempt's
        # input, including when curl fails after transferring a complete body.
        Path(str(destination) + '.tmp').write_text('stale partial installer')
        record, sleeps = root / 'calls.json', root / 'sleeps'
        environment = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
            TEST_RECORD=str(record), TEST_SLEEPS=str(sleeps), TEST_OUTCOMES=json.dumps(outcomes))
        result = subprocess.run(['bash', '-euo', 'pipefail', '-c', function[0] +
            '\ndownload_installer "$1" "$2" "$3"', 'download-test',
            'https://installer.example/toolkit.run', str(destination), 'https://referer.example/'],
            env=environment, capture_output=True, timeout=10)
        calls = json.loads(record.read_text())
        for call in calls:
            args = call['args']
            self.assertFalse(call['temporary_existed'])
            self.assertIn('--http1.1', args)
            self.assertIn('--fail', args)
            self.assertIn('--location', args)
            self.assertFalse(any(arg.startswith('--retry') for arg in args))
            self.assertNotIn('--insecure', args)
            self.assertEqual(args[args.index('--referer') + 1], 'https://referer.example/')
            for option, value in [('--connect-timeout', '20'), ('--max-time', '1800'),
                                  ('--speed-time', '60'), ('--speed-limit', '1024')]:
                self.assertEqual(args[args.index(option) + 1], value)
        self.assertFalse(Path(str(destination) + '.tmp').exists())
        self.assertEqual(sleeps.read_text().splitlines() if sleeps.exists() else [], ['2'] * (len(calls) - 1))
        return result, destination, calls

    def test_http2_stream_error_is_retried_then_promotes_only_success(self):
        for script in SCRIPTS:
            with self.subTest(script=script.name), tempfile.TemporaryDirectory() as directory:
                result, destination, calls = self.invoke(Path(directory), script, [(92, '200'), (0, '200')])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(calls), 2)
                self.assertEqual(destination.read_text(), 'complete installer')
                self.assertEqual(result.stdout, b'', 'HTTP status output escaped the downloader')

    def test_permanent_http_and_certificate_errors_fail_without_retry(self):
        for script in SCRIPTS:
            for outcome in [(22, '403'), (22, '404'), (51, '000'), (60, '000')]:
                with self.subTest(script=script.name, outcome=outcome), tempfile.TemporaryDirectory() as directory:
                    result, destination, calls = self.invoke(Path(directory), script, [outcome])
                    self.assertEqual(result.returncode, outcome[0])
                    self.assertEqual(len(calls), 1)
                    self.assertFalse(destination.exists())

    def test_transport_retries_are_exhausted_after_four_attempts(self):
        for script in SCRIPTS:
            with self.subTest(script=script.name), tempfile.TemporaryDirectory() as directory:
                result, destination, calls = self.invoke(Path(directory), script, [(92, '200')])
                self.assertEqual(result.returncode, 92)
                self.assertEqual(len(calls), 4)
                self.assertFalse(destination.exists())

    def test_success_is_not_repeated(self):
        for script in SCRIPTS:
            with self.subTest(script=script.name), tempfile.TemporaryDirectory() as directory:
                result, destination, calls = self.invoke(Path(directory), script, [(0, '200')])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(calls), 1)
                self.assertEqual(destination.read_text(), 'complete installer')


if __name__ == '__main__':
    unittest.main()
