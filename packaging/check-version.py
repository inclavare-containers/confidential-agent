#!/usr/bin/env python3
import json, os, pathlib, re, subprocess
root = pathlib.Path(__file__).resolve().parent.parent
version = re.search(r'^version = "([^"]+)"', (root/'Cargo.toml').read_text(), re.M)[1]
assert re.fullmatch(r'\d+\.\d+\.\d+', version)
spec = (root/'cagent.spec').read_text()
assert re.search(r'^Version:\s+(\S+)', spec, re.M)[1] == version
assert 'version='+version+'\n' in (root/'packaging/install-rpm.sh').read_text()
assert '"confidential-agent '+version+'"' in (root/'packaging/ci-install-test.sh').read_text()
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root))
assert all(p['version']==version for p in metadata['packages'] if p['id'] in metadata['workspace_members'])
ref = os.environ.get('GITHUB_REF', '')
if ref.startswith('refs/tags/'):
    assert ref == 'refs/tags/v'+version, 'Tag and package version differ'
print('Workspace, RPM, installer and release version: '+version)
