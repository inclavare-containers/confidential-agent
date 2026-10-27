"""Exercise bounded builds, release on failure and credential-free timings."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from command_runner import build_slot
from cloud_case import run


class CommandRunnerTest(unittest.TestCase):
    def test_explicit_builder_mirror_is_applied_before_the_command_without_changing_deploy(self):
        runner = Path(__file__).resolve().parents[1] / 'command_runner.py'
        for label, mirror in [('build', 'https://ecs.example/alinux'), ('deploy', 'https://ecs.example/alinux'), ('build', '')]:
            with self.subTest(label=label, mirror=mirror), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                spec = work / 'spec.json'
                original = {'build': {'image_name': 'fixture', 'repos': [
                    {'name': 'epel', 'baseurl': 'https://epel.example/repo', 'gpgkey': 'https://epel.example/key'}]}}
                spec.write_text(json.dumps(original))
                probe = 'import json,pathlib,sys; print(json.dumps(json.loads(pathlib.Path(sys.argv[-1]).read_text())))'
                environment = dict(os.environ, E2E_WORK_DIR=str(work), E2E_MATRIX_REPO_MIRROR=mirror)
                environment.pop('E2E_REKOR_URL', None)
                result = subprocess.run([sys.executable, str(runner), label, '--', sys.executable,
                    '-c', probe, '--spec', str(spec)], env=environment, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                observed = json.loads(result.stdout)
                if label == 'build' and mirror:
                    repositories = observed['build']['repos']
                    self.assertEqual(repositories[0], original['build']['repos'][0])
                    self.assertEqual(len(repositories), 6)
                    self.assertTrue(all(r['baseurl'].startswith(mirror) and r['gpgkey'].startswith(mirror)
                                        for r in repositories[1:]))
                else:
                    self.assertEqual(observed, original)

    def test_parallel_builds_respect_the_limit_and_release_after_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            active = peak = 0
            mutex = threading.Lock()
            def build(index):
                nonlocal active, peak
                with build_slot(root / (str(index) + '.json')):
                    with mutex:
                        active += 1
                        peak = max(peak, active)
                    try:
                        time.sleep(0.03)
                        if index == 0:
                            raise RuntimeError('simulated failure')
                    finally:
                        with mutex:
                            active -= 1
            with patch.dict(os.environ, E2E_BUILD_SLOT_DIR=str(root / 'slots'), E2E_BUILD_JOBS='2'), \
                    concurrent.futures.ThreadPoolExecutor(5) as pool:
                futures = [pool.submit(build, i) for i in range(5)]
                for index, future in enumerate(futures):
                    if index == 0:
                        with self.assertRaises(RuntimeError):
                            future.result()
                    else:
                        future.result(timeout=5)
            self.assertEqual(active, 0)
            self.assertLessEqual(peak, 2)
            self.assertEqual(len(list(root.glob('*.json'))), 5)

    def test_timings_include_failures_and_timeout_without_command_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            secret = 'test-secret-not-for-timing-output'
            for label, command, timeout in [
                ('success', [sys.executable, '-c', 'pass', secret], 5),
                ('failure', [sys.executable, '-c', 'raise SystemExit(3)', secret], 5),
                ('timeout', [sys.executable, '-c', 'import time; time.sleep(30)', secret], 0.05),
            ]:
                if label == 'success':
                    run(command, root / label, timeout=timeout)
                else:
                    with self.assertRaises(RuntimeError):
                        run(command, root / label, timeout=timeout)
                evidence = (root / (label + '.timing.json')).read_text()
                self.assertNotIn(secret, evidence)
                timing = json.loads(evidence)
                self.assertEqual(timing['timed_out'], label == 'timeout')
                self.assertIsInstance(timing['elapsed_seconds'], (int, float))


if __name__ == '__main__':
    unittest.main()
