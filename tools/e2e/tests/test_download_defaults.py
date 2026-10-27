import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


PATH = Path(__file__).resolve().parents[1] / "render_case.py"
sys.path.insert(0, str(PATH.parent))
SPEC = importlib.util.spec_from_file_location("render_download_defaults", PATH)
RENDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDER)


class DownloadDefaultsTest(unittest.TestCase):
    def context(self, environment):
        environment = {"COSIGN_KEY": "/unused-test-cosign.key", **environment}
        with patch.dict(os.environ, environment, clear=True):
            return RENDER.context_from_env(Path("/unused-e2e-work"))

    def test_cli_and_other_agents_share_mainland_default(self):
        context = self.context({})
        self.assertEqual(context["NPM_REGISTRY"], "https://registry.npmmirror.com/")
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], context["NPM_REGISTRY"])

    def test_shared_registry_override_reaches_cli_agents(self):
        context = self.context({"E2E_NPM_REGISTRY": "https://shared.example/"})
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], "https://shared.example/")

    def test_cli_override_takes_precedence(self):
        context = self.context({"E2E_NPM_REGISTRY": "https://shared.example/",
                                "E2E_CLI_AGENT_NPM_REGISTRY": "https://cli.example/"})
        self.assertEqual(context["NPM_REGISTRY"], "https://shared.example/")
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], "https://cli.example/")

    def test_pep_selection_preserves_installed_and_development_paths(self):
        for environment, expected in [
                ({}, str(RENDER.ROOT / "target/debug/cai-pep")),
                ({"CA_PEP_BIN": "/usr/bin/cai-pep"}, "/usr/bin/cai-pep"),
                ({"CA_PEP_BIN": "/usr/bin/cai-pep", "CAI_PEP": "/selected/cai-pep"}, "/selected/cai-pep")]:
            with self.subTest(environment=environment):
                self.assertEqual(self.context(environment)["CAI_PEP"], expected)

    def test_native_builder_mirror_changes_only_signed_https_repositories(self):
        for style in ['  - name: alinux-os\n    baseurl: https://original.example/os\n    gpgkey: https://original.example/key\n',
                      '    - name: alinux-os\n      baseurl: https://original.example/os\n      gpgkey: https://original.example/key\n']:
            with self.subTest(style=style), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'build.yaml'
                prefix, suffix = 'build:\n  files: []\n', '  scripts: [./install.sh]\nattestation:\n  reference_values: rekor\n'
                original = prefix + '  repos:\n' + style + suffix
                path.write_text(original)
                with patch.dict(os.environ, {}, clear=True):
                    RENDER.configure_repositories(path)
                self.assertEqual(path.read_text(), original)
                with patch.dict(os.environ, {'E2E_MATRIX_REPO_MIRROR': 'https://mirror.example/alinux/'}, clear=True):
                    RENDER.configure_repositories(path)
                output = path.read_text()
                self.assertTrue(output.startswith(prefix) and output.endswith(suffix))
                self.assertEqual(output.count('gpgkey:'), 5)
                for name in ['os', 'updates', 'plus', 'module', 'powertools']:
                    self.assertIn('https://mirror.example/alinux/3/' + name + '/$basearch/', output)
                self.assertNotIn('sslverify', output)
                self.assertNotIn('gpgcheck', output)

    def test_repository_mirror_rejects_insecure_or_credential_urls(self):
        for mirror in ['http://mirror.example', 'https://user:secret@mirror.example',
                       'https://mirror.example?key=secret', 'https://mirror.example#fragment',
                       'https://mirror.example\n', 'https://mirror.example:invalid',
                       'https://mirror.example:99999', 'https://']:
            with self.subTest(mirror=mirror), self.assertRaises(ValueError):
                RENDER.render_matrix_repositories({'E2E_MATRIX_REPO_MIRROR': mirror})

    def configure(self, source, suffix='yaml'):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ('build.' + suffix)
            path.write_text(source)
            with patch.dict(os.environ, {'E2E_MATRIX_REPO_MIRROR': 'https://mirror.example/alinux'}, clear=True):
                RENDER.configure_repositories(path)
                output = path.read_text()
                RENDER.configure_repositories(path)
                self.assertEqual(path.read_text(), output, 'repository configuration is not idempotent')
            return output

    def test_absent_or_empty_yaml_repositories_are_added_without_other_build_edits(self):
        for field in ['', '  repos: []\n', '  repos: null\n']:
            with self.subTest(field=field):
                output = self.configure('schema: confidential-agent/v1\nbuild:\n  image_name: fixture\n' + field
                    + '  scripts: [./install.sh]\nattestation:\n  reference_values: rekor\n')
                self.assertIn('  image_name: fixture\n', output)
                self.assertIn('  scripts: [./install.sh]\n', output)
                self.assertTrue(output.endswith('attestation:\n  reference_values: rekor\n'))
                self.assertEqual(output.count('gpgkey:'), 5)

    def test_yaml_mixed_repositories_keep_epel_and_custom_entries(self):
        for indent in [2, 4]:
            with self.subTest(indent=indent):
                prefix = ' ' * indent
                extra = (prefix + '- name: epel\n' + prefix + '  baseurl: https://epel.example/$basearch/\n'
                    + prefix + '  gpgkey: https://epel.example/key\n'
                    + prefix + '- baseurl: https://custom.example/packages/\n'
                    + prefix + '  name: custom\n' + prefix + '  gpgkey: https://custom.example/key\n')
                canonical = (prefix + '- name: "alinux-os"\n' + prefix + '  baseurl: https://old.example/os\n'
                    + prefix + '  gpgkey: https://old.example/key\n')
                output = self.configure('build:\n  image_name: fixture\n  repos:\n' + extra + canonical
                    + '  scripts: [./install.sh]\nservice:\n  id: fixture\n')
                self.assertIn(extra, output)
                self.assertNotIn('https://old.example/', output)
                self.assertEqual(output.count('gpgkey:'), 7)
                self.assertEqual(output.count('name: alinux-os'), 1)
                self.assertTrue(output.endswith('  scripts: [./install.sh]\nservice:\n  id: fixture\n'))

    def test_json_repositories_preserve_other_sources_and_fields(self):
        extra = {'name': 'epel', 'baseurl': 'https://epel.example/packages/', 'gpgkey': 'https://epel.example/key'}
        for repositories in [None, [], [extra, {'name': 'alinux-os', 'baseurl': 'https://old.example/', 'gpgkey': 'https://old.example/key'}]]:
            with self.subTest(repositories=repositories):
                source = {'build': {'scripts': ['./install.sh']}, 'service': {'id': 'fixture'}}
                if repositories is not None:
                    source['build']['repos'] = repositories
                output = json.loads(self.configure(json.dumps(source), 'json'))
                self.assertEqual(output['service'], source['service'])
                self.assertEqual(output['build']['scripts'], source['build']['scripts'])
                selected = output['build']['repos']
                self.assertEqual(len([repo for repo in selected if repo['name'].startswith('alinux-')]), 5)
                if repositories:
                    self.assertIn(extra, selected)
                self.assertNotIn('https://old.example/', json.dumps(output))

    def test_invalid_mirror_does_not_rewrite_the_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'build.yaml'
            source = 'build:\n  repos: []\n'
            path.write_text(source)
            with patch.dict(os.environ, {'E2E_MATRIX_REPO_MIRROR': 'http://mirror.example'}, clear=True):
                with self.assertRaises(ValueError):
                    RENDER.configure_repositories(path)
            self.assertEqual(path.read_text(), source)

    def test_allinone_repository_defaults_are_public_without_cloud_detection(self):
        template = (RENDER.ROOT / 'examples/allinone/build.yaml').read_text()
        for name in ['os', 'updates', 'plus', 'module', 'powertools']:
            self.assertIn('https://mirrors.aliyun.com/alinux/3/' + name + '/$basearch/', template)
        self.assertEqual(template.count('https://mirrors.aliyun.com/alinux/3/RPM-GPG-KEY-ALINUX-3'), 5)
        with patch.dict(os.environ, {'E2E_MATRIX_REAL_CLOUD': '1', 'E2E_REGION': 'cn-beijing'}, clear=True):
            self.assertEqual(RENDER.render_matrix_repositories(os.environ), '')
