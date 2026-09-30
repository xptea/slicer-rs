#!/usr/bin/env python3
"""Verify corresponding source hashes even when a playback bundle came from cache."""
import argparse
import hashlib
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('platform', choices=['macos', 'linux'])
parser.add_argument('bundle', type=Path)
args = parser.parse_args()
manifest = (args.bundle / 'SOURCE-MANIFEST.txt').read_text()
assert 'MISSING' not in manifest, 'source-incomplete playback bundle'

def verify(path, expected):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    assert digest.hexdigest() == expected, 'source checksum mismatch: ' + str(path)

if args.platform == 'macos':
    indexes = list((args.bundle / 'source').glob('*/SHA256SUMS'))
    assert indexes, 'complete collected source indexes missing'
    for index in indexes:
        for line in index.read_text().splitlines():
            expected, name = line.split('  ', 1)
            assert Path(name).name == name
            verify(index.parent / name, expected)
else:
    # The distro bundler appends source hashes to SOURCE-MANIFEST.txt.
    marker = '# Source material SHA-256\n'
    assert marker in manifest, 'source checksum manifest missing'
    source_rows = 0
    for line in manifest.split(marker, 1)[1].splitlines():
        if line.startswith('#') or not line.strip():
            continue
        expected, name = line.split(None, 1)
        assert len(expected) == 64 and all(c in '0123456789abcdef' for c in expected)
        assert Path(name).name == name
        verify(args.bundle / 'source' / name, expected)
        source_rows += 1
    assert source_rows, 'playback source archives missing'
print('Playback source checksums verified')
