"""Regression coverage for concurrent source collection (no network needed)."""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    'playback_sources', Path(__file__).resolve().parents[1] / 'scripts/collect-playback-sources.py',
)
sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sources)

prepare_spec = importlib.util.spec_from_file_location(
    'prepare_ci', Path(__file__).resolve().parents[1] / 'scripts/prepare-ci.py',
)
prepare_ci = importlib.util.module_from_spec(prepare_spec)
prepare_spec.loader.exec_module(prepare_ci)


class ParallelPreparationTests(unittest.TestCase):
    def test_both_processes_start_before_either_finishes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            child = '''
from pathlib import Path
import sys, time
Path(sys.argv[1]).touch()
deadline = time.monotonic() + 5
while not Path(sys.argv[2]).exists():
    if time.monotonic() > deadline:
        raise SystemExit(1)
    time.sleep(0.01)
'''
            commands = [
                [sys.executable, '-c', child, str(root / 'first'), str(root / 'second')],
                [sys.executable, '-c', child, str(root / 'second'), str(root / 'first')],
            ]
            self.assertEqual(prepare_ci.run_parallel(commands), 0)

    def test_one_failure_stops_the_other_process_and_preserves_exit_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            pid_file = Path(temporary) / 'sibling.pid'
            sibling = '''
from pathlib import Path
import os, sys, time
Path(sys.argv[1]).write_text(str(os.getpid()))
time.sleep(30)
'''
            fail = '''
from pathlib import Path
import sys, time
deadline = time.monotonic() + 5
while not Path(sys.argv[1]).exists():
    if time.monotonic() > deadline:
        raise SystemExit(2)
    time.sleep(0.01)
raise SystemExit(7)
'''
            began = time.monotonic()
            status = prepare_ci.run_parallel([
                [sys.executable, '-c', fail, str(pid_file)],
                [sys.executable, '-c', sibling, str(pid_file)],
            ])
            self.assertEqual(status, 7)
            self.assertLess(time.monotonic() - began, 10)
            with self.assertRaises(ProcessLookupError):
                sources.os.kill(int(pid_file.read_text()), 0)


class LinuxMetadataTests(unittest.TestCase):
    def run_field_reader(self, producer):
        script = (Path(__file__).resolve().parents[1] / 'scripts/bundle-playback-linux.sh').read_text()
        function = re.search(r'apt_package_field_for_version\(\) \{.*?\n\}', script, re.S).group()
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / 'apt-cache'
            executable.write_text('#!/usr/bin/env python3\n' + producer)
            executable.chmod(0o755)
            return subprocess.run(
                ['bash', '-euo', 'pipefail', '-c', function + '\napt_package_field_for_version example 1 Source'],
                env={**os.environ, 'PATH': temporary + os.pathsep + os.environ['PATH']},
                text=True, capture_output=True, timeout=10,
            )

    def test_large_metadata_output_is_consumed_without_sigpipe(self):
        result = self.run_field_reader('''
import signal, sys
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
sys.stdout.write('Package: example\\nVersion: 2\\nSource: wrong\\n\\n')
sys.stdout.write('Package: example\\nVersion: 1\\nSource: correct (1)\\n\\n')
sys.stdout.write('Package: example\\nVersion: 1\\nSource: duplicate\\nDescription: ' + 'x' * 1000000 + '\\n')
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'correct (1)\n')

    def test_metadata_command_failure_is_preserved(self):
        result = self.run_field_reader('raise SystemExit(5)\n')
        self.assertEqual(result.returncode, 5)


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
