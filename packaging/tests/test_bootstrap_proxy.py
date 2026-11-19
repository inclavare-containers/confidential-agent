import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BootstrapProxyTests(unittest.TestCase):
    def test_all_entrypoints_proxy_fallback_and_disable(self):
        for name in ['install.sh','install-openclaw-vllm.sh','install-vllm-inference.sh']:
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

    def test_raw_frontend_follows_git_proxy_order(self):
        for name in ('install.sh', 'install-openclaw-vllm.sh', 'install-vllm-inference.sh'):
            for first, proxy, expected in (
                ('1', 'https://proxy.invalid/', ['proxy', 'direct']),
                ('0', 'https://proxy.invalid', ['direct', 'proxy']),
                ('1', '', ['direct']),
            ):
                with self.subTest(script=name, first=first, proxy=proxy), tempfile.TemporaryDirectory() as tmp:
                    directory = Path(tmp)
                    curl = directory / 'curl'
                    curl.write_text('''#!/usr/bin/env python3.11
import os
from pathlib import Path
import shutil
import sys

args = sys.argv[1:]
target = Path(args[args.index('-o') + 1])
url = args[-1]
route = 'proxy' if url.startswith('https://proxy.invalid/') else 'direct'
origin = 'https://raw.githubusercontent.com/inclavare-containers/confidential-agent/'
relative = url.split(origin, 1)[1].split('/', 1)[1]
filename = Path(relative).name
if filename in ('bootstrap-frontend.sh', 'credentials.sh', os.environ['TEST_ENTRYPOINT']):
    with open(os.environ['TEST_ROUTES'], 'a') as routes:
        print(filename, route, file=routes)
    if route == os.environ['TEST_FIRST_FAILURE']:
        raise SystemExit(22)
if relative == 'one-click/lib/main.sh':
    target.write_text('echo "$CA_ONE_CLICK_ENTRY_URL"\\n')
else:
    shutil.copyfile(Path(os.environ['TEST_REPO']) / relative, target)
''')
                    curl.chmod(0o755)
                    routes = directory / 'routes'
                    env = dict(
                        os.environ,
                        PATH=f'{directory}:{os.environ["PATH"]}',
                        CA_GITHUB_PROXY_FIRST=first,
                        CA_GITHUB_PROXY_URL=proxy,
                        TEST_ENTRYPOINT=name,
                        TEST_ROUTES=str(routes),
                        TEST_FIRST_FAILURE=expected[0] if proxy else '',
                        TEST_REPO=str(ROOT),
                    )
                    for name_inherited in ('REPO', 'REF', 'BRANCH', 'SOURCE_DIR'):
                        env.pop(f'CA_ONE_CLICK_{name_inherited}', None)
                    result = subprocess.run(
                        ['sh', '-s', '--', 'deploy'], input=(ROOT / 'one-click' / name).read_text(),
                        text=True, capture_output=True, cwd=directory, env=env,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(routes.read_text().splitlines(), [
                        f'{filename} {route}'
                        for filename in ('bootstrap-frontend.sh', 'credentials.sh', name)
                        for route in expected
                    ])
                    self.assertEqual(result.stdout.startswith('https://proxy.invalid/'), expected[-1] == 'proxy')


if __name__=='__main__':unittest.main()
