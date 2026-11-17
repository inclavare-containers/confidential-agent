import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
INSTALLER = ROOT / 'examples/openclaw-vllm/install-openclaw-vllm.sh'


def executable(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('#!/bin/sh\nset -e\n' + body + '\n')
    path.chmod(0o755)


class LegacyVllmRuntimeToolsTest(unittest.TestCase):
    def test_dependency_install_creates_home_in_minimal_image(self):
        for installer in [INSTALLER, ROOT / 'examples/vllm-inference/install-vllm-inference.sh']:
            with self.subTest(installer=installer.name), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                home = work / 'root'
                headers = work / 'include'
                (headers / 'python3.11').mkdir(parents=True)
                (headers / 'python3.11/Python.h').touch()
                (headers / 'stdio.h').touch()
                script = installer.read_text().split(
                    "cat >/usr/local/bin/cai-vllm-install-deps.sh <<'EOF'\n", 1)[1].split('\nEOF', 1)[0]
                script = script.replace('/root', str(home)).replace('/usr/include', str(headers))
                script = script.replace('/etc/uv', str(work / 'etc/uv'))
                script = script.replace('export PATH=', 'export PATH=' + shlex.quote(str(work / 'bin')) + ':', 1)
                executable(work / 'bin/python3.11', 'exit 0')
                executable(work / 'bin/uv', '''
test "$PWD" = "$TEST_HOME"
case "$1" in
  venv) mkdir -p .venv/bin ;;
  pip) test -d .venv/bin ;;
  *) exit 1 ;;
esac
printf '%s:%s\\n' "$1" "$PWD" >> "$TEST_UV_LOG"
''')
                environment = dict(os.environ, TEST_HOME=str(home), TEST_UV_LOG=str(work / 'uv.log'),
                                   PIP_CACHE_DIR=str(work / 'cache/pip'),
                                   UV_CACHE_DIR=str(work / 'cache/uv'), XDG_CACHE_HOME=str(work / 'cache/xdg'))
                self.assertFalse(home.exists())
                result = subprocess.run(['bash', '-c', script], env=environment,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(home.stat().st_mode & 0o777, 0o700)
                self.assertEqual((work / 'uv.log').read_text().splitlines(),
                                 ['venv:' + str(home), 'pip:' + str(home)])

    def test_existing_venv_without_ninja_is_not_ready(self):
        script = INSTALLER.read_text()
        start = script.index('vllm_deps_ready() {')
        function = script[start:script.index('\n}\nrestore_vllm_build_headers', start) + 2]
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            venv = work / 'venv'
            for path in [venv / 'bin/python', venv / 'bin/vllm', work / 'bin/python3.11']:
                executable(path, 'exit 0')
            function = function.replace('/root/.venv', str(venv))
            environment = dict(os.environ, VLLM_VERSION='0.28.0',
                               PATH=str(work / 'bin') + ':' + os.environ['PATH'])
            command = ['bash', '-c', function + '\nvllm_deps_ready\n']
            self.assertNotEqual(subprocess.run(command, env=environment).returncode, 0)
            executable(venv / 'bin/ninja', 'exit 0')
            self.assertEqual(subprocess.run(command, env=environment).returncode, 0)

    def test_generated_service_keeps_jit_tools_and_mamba_scheduler_valid(self):
        script = INSTALLER.read_text()
        unit = script.split("cat >/etc/systemd/system/cai-vllm.service <<'EOF'\n", 1)[1].split('\nEOF', 1)[0]
        runtime_path = next(line.removeprefix('Environment=PATH=') for line in unit.splitlines()
                            if line.startswith('Environment=PATH='))
        start = script.index('cat >/usr/local/bin/cai-vllm-run.sh <<EOF\n')
        generate = script[start:script.index('\nEOF', start) + 4]
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            venv = work / 'venv/bin'
            cuda = work / 'cuda/bin'
            # The real 96 GiB GPU exposed 801 Mamba cache blocks; vLLM's
            # default scheduler requested 1024 and failed before serving.
            executable(venv / 'vllm', f'''{shlex.quote(sys.executable)} - "$@" <<'PY'
import argparse
parser = argparse.ArgumentParser()
parser.add_argument('--max-num-seqs', type=int, default=1024)
args, _ = parser.parse_known_args()
if args.max_num_seqs > 801:
    raise SystemExit('max_num_seqs exceeds available Mamba cache blocks')
PY
ninja --version
nvcc --version''')
            executable(venv / 'ninja', 'echo NINJA_READY')
            executable(cuda / 'nvcc', 'echo CUDA_READY')
            header = work / 'Python.h'
            header.touch()
            generate = generate.replace('/usr/local/bin/cai-vllm-run.sh', str(work / 'run.sh'))
            generate = generate.replace('/root/.venv/bin', str(venv))
            generate = generate.replace('/usr/include/python3.11/Python.h', str(header))
            generate = generate.replace('cd /root', 'cd ' + shlex.quote(str(work)))
            environment = dict(os.environ, MODEL_DIR=str(work / 'model'), VLLM_PORT='8090',
                               SERVED_MODEL_NAME='test-model')
            subprocess.run(['bash', '-c', generate], env=environment, check=True)
            environment['PATH'] = runtime_path.replace('/root/.venv/bin', str(venv)).replace('/usr/local/cuda/bin', str(cuda))
            unbounded = subprocess.run([str(venv / 'vllm'), 'serve', 'model'], env=environment,
                                       capture_output=True, text=True)
            self.assertNotEqual(unbounded.returncode, 0)
            self.assertIn('Mamba cache blocks', unbounded.stderr)
            result = subprocess.run(['bash', str(work / 'run.sh')], env=environment,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['NINJA_READY', 'CUDA_READY'])


if __name__ == '__main__':
    unittest.main()
