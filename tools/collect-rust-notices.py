#!/usr/bin/env python3
"""Collect dependency license notices from the exact Cargo.lock dependency graph."""
import json
import pathlib
import subprocess
import sys

output = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'build/rust-notices')
output.mkdir(parents=True, exist_ok=True)
# Metadata includes dependencies for other platforms that a native build does
# not download. Allow Cargo to fetch those exact locked crates for their notices.
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1']))
lines = ['# Rust dependency notices', '', 'Generated from Cargo.lock. Each package retains its own license.', '']
for package in sorted(metadata['packages'], key=lambda item: (item['name'], item['version'])):
    if package['source'] is None:
        continue
    label = f"{package['name']}-{package['version']}"
    lines += [f"## {label}", '', f"License: {package.get('license') or 'See package license file'}", f"Source: {package.get('repository') or 'https://crates.io/crates/' + package['name']}", '']
    root = pathlib.Path(package['manifest_path']).parent
    licenses = set()
    for pattern in ['LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*', 'license*', 'licenses/*']:
        licenses.update(path for path in root.glob(pattern) if path.is_file())
    if package.get('license_file'):
        path = root / package['license_file']
        if path.is_file():
            licenses.add(path)
    destination = output / label
    destination.mkdir(exist_ok=True)
    for path in sorted(licenses):
        name = str(path.relative_to(root)).replace('/', '_')
        (destination / name).write_bytes(path.read_bytes())
        lines.append(f'- [{name}]({label}/{name})')
    lines.append('')
(output / 'README.md').write_text('\n'.join(lines))
print(output)
