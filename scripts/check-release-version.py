#!/usr/bin/env python3
"""Reject release tags and update feeds that do not match the built application."""
import argparse
import json
from pathlib import Path
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tag')
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
package = (root / 'Cargo.toml').read_text().split('[package]', 1)[1].split('\n[', 1)[0]
local = re.search(r'^version\s*=\s*"([^"]+)"', package, re.M)[1]
feed = json.loads((root / 'version.json').read_text())
assert re.fullmatch(r'\d+\.\d+\.\d+', local), 'releases must use a stable MAJOR.MINOR.PATCH version'
assert feed['version'] == local, 'version.json and Cargo.toml versions differ'
assert re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', feed['repository']), 'invalid releases repository'
assert re.fullmatch(r'[A-Za-z0-9_.-]+', feed['branch']), 'invalid update branch'
if args.tag:
    assert args.tag == 'v' + local, 'publish a release tagged v' + local
print('Release version:', local)
