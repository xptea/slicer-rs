"""Regression coverage for concurrent source collection (no network needed)."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    'playback_sources', Path(__file__).resolve().parents[1] / 'scripts/collect-playback-sources.py',
)
sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sources)


class LinuxSourceCollectionTests(unittest.TestCase):
    def inventory(self, directory, packages):
        bundle = directory / 'bundle'
        bundle.mkdir()
        (bundle / 'SOURCE-MANIFEST.txt').write_text(
            '# Source inventory\n' + ''.join(
                f'libexample | 1 | hash | {package} | {version} | amd64 | notice | MISSING\n'
                for package, version in packages
            ),
        )
        return bundle

    def test_parallel_downloads_are_isolated_and_materials_are_flattened(self):
        # Two source versions share the same upstream archive, just as distro
        # packages do. Both must finish without competing in one apt directory.
        barrier = threading.Barrier(2)
        directories = set()
        lock = threading.Lock()

        def apt(command, *, cwd, **kwargs):
            with lock:
                directories.add(cwd)
            barrier.wait(timeout=5)
            version = command[-1].split('=', 1)[1]
            (cwd / 'example_1.orig.tar.gz').write_bytes(b'upstream')
            (cwd / f'example_{version.replace(":", "_")}.dsc').write_text(version)
            return subprocess.CompletedProcess(command, 0, stdout='Downloaded\n')

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = self.inventory(root, [('example', '1:1-1'), ('example', '1:1-2'), ('example', '1:1-1')])
            cache = root / 'cache'
            with patch.object(sources.subprocess, 'run', side_effect=apt) as run:
                sources.linux(bundle, cache, jobs=2)
            self.assertEqual(run.call_count, 2)  # repeated binary owners deduplicate
            self.assertEqual(len(directories), 2)
            self.assertEqual((cache / 'example_1.orig.tar.gz').read_bytes(), b'upstream')
            self.assertEqual((cache / 'example_1_1-1.dsc').read_text(), '1:1-1')
            self.assertEqual((cache / 'example_1_1-2.dsc').read_text(), '1:1-2')

    def test_failed_download_retains_files_for_retry_and_propagates_error(self):
        def failed_apt(command, *, cwd, **kwargs):
            (cwd / 'archive.partial').write_bytes(b'partial download')
            raise subprocess.CalledProcessError(100, command, output='source unavailable\n')

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = self.inventory(root, [('example', '1-1')])
            cache = root / 'cache'
            with patch.object(sources.subprocess, 'run', side_effect=failed_apt):
                with self.assertRaises(subprocess.CalledProcessError):
                    sources.linux(bundle, cache)
            partial = cache / 'example-1-1/archive.partial'
            self.assertEqual(partial.read_bytes(), b'partial download')

            def retry_apt(command, *, cwd, **kwargs):
                self.assertEqual((cwd / 'archive.partial').read_bytes(), b'partial download')
                (cwd / 'archive.partial').unlink()
                (cwd / 'example_1.orig.tar.gz').write_bytes(b'complete download')
                return subprocess.CompletedProcess(command, 0, stdout='Resumed\n')

            with patch.object(sources.subprocess, 'run', side_effect=retry_apt):
                sources.linux(bundle, cache)
            self.assertEqual((cache / 'example_1.orig.tar.gz').read_bytes(), b'complete download')

    def test_timeout_is_a_build_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = self.inventory(root, [('example', '1-1')])
            with patch.object(sources.subprocess, 'run', side_effect=subprocess.TimeoutExpired('apt-get', 900)):
                with self.assertRaises(subprocess.TimeoutExpired):
                    sources.linux(bundle, root / 'cache')

    def test_empty_inventory_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(RuntimeError, 'no distro source packages'):
                sources.linux(self.inventory(root, []), root / 'cache')


if __name__ == '__main__':
    unittest.main()
