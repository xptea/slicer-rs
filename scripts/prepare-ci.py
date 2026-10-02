#!/usr/bin/env python3
"""Compile Rust while preparing playback inputs on the same CI runner."""
import os
import signal
import subprocess
import sys
import time


RUST_BUILD = ['bash', '-euo', 'pipefail', '-c', '''
cargo build --release --locked
cargo test --release --locked --no-run
''']

PLAYBACK_BUILD = ['bash', '-euo', 'pipefail', '-c', '''
if [[ "$PLATFORM" == macos ]]; then
  python3 scripts/bundle-playback-macos.py --libmpv "$(brew --prefix mpv)/lib/libmpv.2.dylib" --output build/playback-inventory --allow-missing-sources
  python3 scripts/collect-playback-sources.py macos build/playback-inventory build/playback-source
  python3 scripts/bundle-playback-macos.py --libmpv "$(brew --prefix mpv)/lib/libmpv.2.dylib" --output "build/playback/$TARGET" --source-cache build/playback-source --require-source-index
else
  scripts/bundle-playback-linux.sh --output build/playback-inventory --allow-missing-sources
  python3 scripts/collect-playback-sources.py linux build/playback-inventory build/playback-source
  scripts/bundle-playback-linux.sh --output "build/playback/$TARGET" --source-cache build/playback-source
fi
''']


def run_parallel(commands):
    processes = []
    try:
        for command in commands:
            processes.append(subprocess.Popen(command, start_new_session=True))
        while True:
            statuses = [process.poll() for process in processes]
            failure = next((status for status in statuses if status not in (None, 0)), None)
            if failure is not None:
                return failure if failure > 0 else 1
            if all(status == 0 for status in statuses):
                return 0
            time.sleep(0.1)
    finally:
        # Stop complete process groups on failure/cancellation, including rustc
        # and downloader children, before the workflow saves its caches.
        for process in processes:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for process in processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()


def main():
    if os.environ.get('PLATFORM') not in ('linux', 'macos') or not os.environ.get('TARGET'):
        raise SystemExit('PLATFORM and TARGET must identify a supported CI build')
    # Make a cancelled Actions step clean up its child process groups too.
    def cancelled(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, cancelled)
    commands = [RUST_BUILD]
    if os.environ.get('SLICER_PLAYBACK_CACHED') != 'true':
        commands.append(PLAYBACK_BUILD)
    return run_parallel(commands)


if __name__ == '__main__':
    sys.exit(main())
