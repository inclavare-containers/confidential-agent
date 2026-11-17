import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BootstrapProxyTests(unittest.TestCase):
    def test_unified_bootstrap_forwards_target_and_arguments(self):
        for target in ['openclaw', 'openclaw-vllm', 'vllm-inference']:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'one-click/lib').mkdir(parents=True)
                (root / 'Cargo.toml').touch()
                entry = root / 'one-click/install.sh'
                entry.write_text((ROOT / 'one-click/install.sh').read_text())
                (root / 'one-click/lib/main.sh').write_text('printf "%s\\n" "$CA_ONE_CLICK_TARGET" "$@"\n')
                result = subprocess.run(['sh', str(entry), '--target', target, 'deploy', '--model', 'model with spaces'],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), [target, 'deploy', '--model', 'model with spaces'])

    def test_all_entrypoints_proxy_fallback_and_disable(self):
        for name in ['install.sh']:
            source=(ROOT/'one-click'/name).read_text().removesuffix('main "$@"\n')
            for first, proxy, fail, expected in [
                ('1','https://proxy.invalid/','proxy', ['proxy','direct']),
                ('0','https://proxy.invalid/','direct', ['direct','proxy']),
                ('1','','none',['direct']),
            ]:
                with self.subTest(script=name,first=first,proxy=proxy), tempfile.TemporaryDirectory() as temp:
                    code=source+'''
git() {
  case "$4" in
    set-url) printf '%s' "$6" > "$TEST_DIR/current" ;;
    *) echo "unexpected fake git arguments: $*" >&2; return 9 ;;
  esac
}
run_git_with_timeout() {
  url="$(cat "$TEST_DIR/current")"
  case "$url" in https://proxy.invalid/*) kind=proxy ;; *) kind=direct ;; esac
  printf '%s\\n' "$kind" >> "$TEST_DIR/attempts"
  [ "$FAIL_KIND" != "$kind" ]
}
git_fetch_with_fallback "$TEST_DIR" https://github.com/example/repo.git master
'''
                    env=dict(os.environ,CA_GITHUB_PROXY_FIRST=first,CA_GITHUB_PROXY_URL=proxy,TEST_DIR=temp,FAIL_KIND=fail)
                    p=subprocess.run(['sh'],input=code,env=env,capture_output=True,text=True)
                    self.assertEqual(p.returncode,0,p.stderr)
                    self.assertEqual((Path(temp)/'attempts').read_text().splitlines(),expected)


if __name__=='__main__':unittest.main()
