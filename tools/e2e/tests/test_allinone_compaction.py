"""Compaction must preserve executable paths, bytes and privileges."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    'allinone_compaction', ROOT / 'examples/allinone/files/finalize-runtime.py')
COMPACTION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPACTION)


class CompactionTests(unittest.TestCase):
    def test_identical_binaries_share_storage_and_keep_every_entrypoint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary, baseline, entry = (root / name for name in ['native', 'baseline', 'entry'])
            contents = b'unchanged native payload\0' * 4096
            primary.write_bytes(contents)
            baseline.write_bytes(contents)
            os.link(baseline, entry)
            for path in [primary, baseline]:
                path.chmod(0o755)
            report = COMPACTION.deduplicate([root])
            self.assertGreater(report['removed_allocated_bytes'], 0)
            for path in [primary, baseline, entry]:
                self.assertEqual(path.read_bytes(), contents)
                self.assertEqual(path.stat().st_mode & 0o777, 0o755)
                self.assertTrue(os.path.samefile(path, primary))
            self.assertEqual(COMPACTION.deduplicate([root])['files_relinked'], 0)

    def test_different_payloads_permissions_and_symlinks_are_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            one, two, private = (root / name for name in ['one', 'two', 'private'])
            one.write_bytes(b'a' * 65536)
            two.write_bytes(b'b' * 65536)
            one.chmod(0o644)
            two.chmod(0o644)
            private.write_bytes(one.read_bytes())
            private.chmod(0o600)
            (root / 'alias').symlink_to(one)
            outside = root / 'outside'
            outside.mkdir()
            (outside / 'copy').write_bytes(one.read_bytes())
            (outside / 'copy').chmod(0o644)
            (root / 'directory-alias').symlink_to(outside, target_is_directory=True)
            report = COMPACTION.deduplicate([root / 'directory-alias'])
            self.assertEqual(report['files_relinked'], 0)
            # Only the equal public copy may be shared; neither permissions nor
            # the symlink itself can be replaced by a hard link.
            COMPACTION.deduplicate([root])
            self.assertFalse(os.path.samefile(one, two))
            self.assertFalse(os.path.samefile(one, private))
            self.assertTrue((root / 'alias').is_symlink())
            self.assertEqual(private.stat().st_mode & 0o777, 0o600)

    def test_extended_attributes_prevent_privilege_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            one, two = root / 'one', root / 'two'
            one.write_bytes(b'a' * 65536)
            two.write_bytes(one.read_bytes())
            try:
                os.setxattr(one, 'user.test', b'original')
            except OSError as error:
                self.skipTest(str(error))
            self.assertEqual(COMPACTION.deduplicate([root])['files_relinked'], 0)
            self.assertEqual(os.getxattr(one, 'user.test'), b'original')

    def test_cache_accounting_excludes_hardlinks_into_the_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / 'cache'
            cache.mkdir()
            runtime = root / 'runtime'
            runtime.write_bytes(b'x' * 65536)
            os.link(runtime, cache / 'shared')
            self.assertEqual(COMPACTION.exclusive_allocated_bytes(cache), 0)
            disposable = cache / 'disposable'
            disposable.write_bytes(b'y' * 65536)
            self.assertEqual(COMPACTION.exclusive_allocated_bytes(cache),
                             disposable.stat().st_blocks * 512)

    def test_node_cache_requires_an_independent_verified_active_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / 'cache'
            (cache / 'bin').mkdir(parents=True)
            cached = cache / 'bin/node'
            cached.write_bytes(b'node payload')
            active = root / 'node'
            active.symlink_to(cached)
            result = COMPACTION.remove_node_cache(cache, active, [active])
            self.assertEqual(result['status'], 'preserved-active-runtime')
            self.assertTrue(cached.exists())
            active.unlink()
            active.write_bytes(b'other node version')
            self.assertEqual(COMPACTION.remove_node_cache(cache, active, [active])['status'],
                             'preserved-unverified-copy')
            active.write_bytes(cached.read_bytes())
            self.assertEqual(COMPACTION.remove_node_cache(cache, active, [active])['status'],
                             'removed')
            self.assertEqual(active.read_bytes(), b'node payload')
            self.assertFalse(cache.exists())


if __name__ == '__main__':
    unittest.main()
