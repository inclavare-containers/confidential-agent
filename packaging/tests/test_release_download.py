import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('fetch_shelter', ROOT/'hack/fetch-shelter-rpm.py')
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)


@unittest.skipUnless(shutil.which('rpm') and shutil.which('rpmbuild'), 'RPM tools required')
class ReleaseDownloadTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.base = Path(cls.temp.name)
        cls.rpms = {}
        for name, version, release in [('shelter','0.3.2','2.al8'),('cagent','0.2.0','1.al8')]:
            specfile = cls.base/(name+'.spec')
            specfile.write_text(f'''Name: {name}
Version: {version}
Release: {release}
Summary: Release test fixture
License: MIT
BuildArch: x86_64
%description
Fixture only.
%install
mkdir -p %{{buildroot}}/usr/share/release-fixture
printf fixture > %{{buildroot}}/usr/share/release-fixture/{name}
%files
/usr/share/release-fixture/{name}
''')
            subprocess.run(['rpmbuild','-bb','--quiet','--define','_topdir '+str(cls.base/'build'),str(specfile)],check=True,capture_output=True)
            cls.rpms[name] = cls.base/'build/RPMS/x86_64'/f'{name}-{version}-{release}.x86_64.rpm'
        cls.manifest = {'file':cls.rpms['shelter'].name,'sha256':fetch.digest(cls.rpms['shelter']),
                        'payload_sha256':subprocess.check_output(['rpm','-qp','--qf','%{PAYLOADDIGEST}',str(cls.rpms['shelter'])],text=True),
                        'url':'https://github.com/example/project/releases/download/v0.2.0/'+cls.rpms['shelter'].name}

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.temp_case = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_case.cleanup)
        self.work = Path(self.temp_case.name)
        self.manifest_path = self.work/'shelter-rpm.json'
        self.manifest_path.write_text(json.dumps(self.manifest))

    def fake_download(self, args, **kwargs):
        self.assertEqual(args[0], 'curl')
        shutil.copy2(self.rpms['shelter'], args[args.index('-o')+1])
        return subprocess.CompletedProcess(args,0)

    def test_download_then_verified_cache_without_network(self):
        # Patch only curl; leave rpm's actual header/payload query intact.
        original = subprocess.run
        def run(args, **kwargs):
            return self.fake_download(args, **kwargs) if args[0]=='curl' else original(args, **kwargs)
        with patch.object(fetch.subprocess,'run',side_effect=run):
            path = fetch.resolve(self.manifest_path,self.work/'cache')
        self.assertEqual(fetch.digest(path),self.manifest['sha256'])
        def cached_run(args, **kwargs):
            self.assertNotIn(args[0], ['curl', 'gh'], 'network unexpectedly used')
            return original(args, **kwargs)
        with patch.object(fetch.subprocess,'run',side_effect=cached_run):
            self.assertEqual(fetch.resolve(self.manifest_path,self.work/'cache'),path)

    def test_bad_download_never_populates_cache(self):
        original = subprocess.run
        def run(args, **kwargs):
            if args[0] == 'curl':
                Path(args[args.index('-o')+1]).write_bytes(b'corrupted download')
                return subprocess.CompletedProcess(args, 0)
            return original(args, **kwargs)
        with patch.object(fetch.subprocess,'run',side_effect=run):
            with self.assertRaisesRegex(ValueError, 'Unable to download'):
                fetch.resolve(self.manifest_path, self.work/'cache')
        self.assertFalse((self.work/'cache'/self.manifest['sha256']/self.manifest['file']).exists())

    def test_corrupt_cached_package_rejected(self):
        path=self.work/'cache'/self.manifest['sha256']/self.manifest['file'];path.parent.mkdir(parents=True)
        path.write_bytes(b'wrong package')
        with self.assertRaisesRegex(ValueError,'SHA256 mismatch'):
            fetch.resolve(self.manifest_path,self.work/'cache')

    def test_matching_file_hash_does_not_bypass_payload_check(self):
        manifest=dict(self.manifest,payload_sha256='0'*64)
        self.manifest_path.write_text(json.dumps(manifest))
        shutil.copy2(self.rpms['shelter'],self.work/self.manifest['file'])
        with self.assertRaisesRegex(ValueError,'payload digest mismatch'):
            fetch.resolve(self.manifest_path,self.work/'cache')

    def test_path_traversal_rejected_before_network(self):
        self.manifest_path.write_text(json.dumps(dict(self.manifest,file='../outside.rpm')))
        with self.assertRaisesRegex(ValueError,'filename'):
            fetch.resolve(self.manifest_path,self.work/'cache')

    def prepare_release(self):
        source=self.work/'release';source.mkdir();packages=[]
        for name,path in self.rpms.items():
            shutil.copy2(path,source/path.name)
            version,release=subprocess.check_output(['rpm','-qp','--qf','%{VERSION}\n%{RELEASE}',str(path)],text=True).splitlines()
            packages.append({'name':name,'version':version,'release':release,'arch':'x86_64','file':path.name,
                             'sha256':fetch.digest(path),'payload_sha256':subprocess.check_output(['rpm','-qp','--qf','%{PAYLOADDIGEST}',str(path)],text=True)})
        (source/'release-manifest.json').write_text(json.dumps({'schema':'confidential-agent/binary-release/v1','version':'0.2.0','tag':'v0.2.0','platform':'alinux3-x86_64','packages':packages}))
        return source

    def test_offline_installer_verifies_pair_and_rejects_tampering(self):
        source=self.prepare_release()
        cmd=['bash',str(ROOT/'packaging/install-rpm.sh'),'--from-dir',str(source),'--download-only','--output-dir',str(self.work/'download')]
        subprocess.run(cmd,capture_output=True,check=True)
        (source/self.rpms['shelter'].name).write_bytes(b'corrupted')
        result=subprocess.run(cmd,capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('FAILED',result.stderr)

    def test_installer_proxy_fallback_and_global_preferences(self):
        source=self.prepare_release()
        bindir=self.work/'bin';bindir.mkdir()
        curl=bindir/'curl'
        curl.write_text("""#!/usr/bin/env python3
import os,pathlib,shutil,sys
args=sys.argv[1:];url=args[-1];name=url.rsplit('/',1)[-1];proxy=url.startswith('https://proxy.invalid/')
with open(os.environ['ATTEMPT_LOG'],'a') as f:f.write(('proxy' if proxy else 'direct')+' '+name+'\\n')
mode=os.environ['FAIL_MODE'];out=pathlib.Path(args[args.index('-o')+1])
if (mode=='proxy_down' and proxy) or (mode=='direct_down' and not proxy):sys.exit(28)
if mode=='proxy_corrupt' and proxy:out.write_text('<html>proxy error</html>')
else:shutil.copy2(pathlib.Path(os.environ['FIXTURE_RELEASE'])/name,out)
""")
        curl.chmod(0o755)
        scenarios=[('proxy_down','1','https://proxy.invalid/',True),
                   ('proxy_corrupt','1','https://proxy.invalid/',True),
                   ('direct_down','0','https://proxy.invalid/',True),
                   ('none','0','https://proxy.invalid/',False),
                   ('none','1','',False)]
        for i,(mode,first,proxy,two_attempts) in enumerate(scenarios):
            with self.subTest(mode=mode,first=first,proxy=proxy):
                log=self.work/('attempts-'+str(i));out=self.work/('download-'+str(i))
                env=dict(os.environ,PATH=str(bindir)+os.pathsep+os.environ['PATH'],FIXTURE_RELEASE=str(source),
                         ATTEMPT_LOG=str(log),FAIL_MODE=mode,CA_GITHUB_PROXY_FIRST=first,CA_GITHUB_PROXY_URL=proxy)
                cmd=['bash',str(ROOT/'packaging/install-rpm.sh'),'--download-only','--output-dir',str(out)]
                result=subprocess.run(cmd,env=env,capture_output=True,text=True)
                self.assertEqual(result.returncode,0,result.stderr)
                attempts=log.read_text().splitlines()
                self.assertEqual(len(attempts),6 if two_attempts else 3)
                expected_first='proxy' if first=='1' and proxy else 'direct'
                self.assertTrue(attempts[0].startswith(expected_first+' '))
                self.assertEqual(fetch.digest(out/self.rpms['shelter'].name),self.manifest['sha256'])


if __name__=='__main__':unittest.main()
