#!/usr/bin/env python3
"""Bundle an explicit libmpv Mach-O closure with private loader-relative paths."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def dependencies(path):
    return [line.strip().split(' (compatibility version', 1)[0]
            for line in run('otool', '-L', str(path)).splitlines()[1:]]


def system(path):
    return path.startswith(('/usr/lib/', '/System/Library/'))


def verify_source_index(directory):
    index = directory / 'SHA256SUMS'
    if not index.is_file():
        raise RuntimeError(f'complete collected source index missing: {index}')
    for row in index.read_text().splitlines():
        expected, name = row.split('  ', 1)
        if Path(name).name != name:
            raise RuntimeError('source index must use local filenames')
        path = directory / name
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise RuntimeError(f'collected source checksum mismatch: {path}')


def check(directory, arch):
    libraries = list(directory.glob('*.dylib'))
    if not (directory / 'libmpv.2.dylib').is_file():
        raise RuntimeError('playback bundle lacks libmpv.2.dylib')
    for library in libraries:
        if arch not in run('lipo', '-archs', str(library)).split():
            raise RuntimeError(f'{library} does not contain {arch}')
        for dep in dependencies(library):
            if dep == '@rpath/' + library.name:  # Mach-O install ID
                continue
            if system(dep):
                continue
            if not dep.startswith('@loader_path/') or not (directory / dep.removeprefix('@loader_path/')).is_file():
                raise RuntimeError(f'{library.name} has an unbundled dependency: {dep}')
        subprocess.run(['codesign', '--verify', '--strict', str(library)], check=True)


def resolve(dep, owner, root):
    if dep.startswith('@loader_path/'):
        candidate = owner.parent / dep.removeprefix('@loader_path/')
    elif dep.startswith('@rpath/'):
        name = dep.removeprefix('@rpath/')
        candidates = [owner.parent / name, root.parent / name]
        lines = run('otool', '-l', str(owner)).splitlines()
        for i, line in enumerate(lines):
            if line.strip() == 'cmd LC_RPATH':
                rpath = lines[i + 2].strip().removeprefix('path ').split(' (offset', 1)[0]
                rpath = rpath.replace('@loader_path', str(owner.parent))
                if '@' not in rpath:
                    candidates.append(Path(rpath) / name)
        candidate = next((p for p in candidates if p.is_file()), None)
        if candidate is None:
            raise RuntimeError(f'cannot resolve {dep} from {owner}')
    elif dep.startswith('@'):
        raise RuntimeError(f'unsupported dependency {dep} in {owner}')
    else:
        candidate = Path(dep)
    if not candidate.is_file():
        raise RuntimeError(f'dependency missing: {candidate}')
    return candidate.resolve()


def bundle(args):
    source = args.libmpv.resolve(strict=True)
    output = args.output.resolve()
    if output.exists():
        raise RuntimeError(f'output already exists: {output}')
    # Inspect the full closure before writing anything.
    queue = [(source, 'libmpv.2.dylib')]
    inputs, edges, names = {}, {}, {}
    while queue:
        owner, name = queue.pop()
        if owner in inputs:
            continue
        if name in names and names[name] != owner:
            raise RuntimeError(f'conflicting dylib basenames: {name}')
        if args.arch not in run('lipo', '-archs', str(owner)).split():
            raise RuntimeError(f'{owner} does not contain {args.arch}')
        names[name], inputs[owner] = owner, name
        own_id = run('otool', '-D', str(owner)).splitlines()[1:]
        edges[owner] = []
        for dep in dependencies(owner):
            if dep in own_id or system(dep):
                continue
            child = resolve(dep, owner, source)
            child_name = 'libmpv.2.dylib' if child == source else Path(dep).name
            edges[owner].append((dep, child_name))
            queue.append((child, child_name))
    output.mkdir(parents=True)
    records = []
    bottles = set()
    for owner, name in inputs.items():
        target = output / name
        shutil.copyfile(owner, target)
        target.chmod(0o755)
        changes = ['install_name_tool', '-id', '@rpath/' + name]
        for dep, child in edges[owner]:
            changes.extend(['-change', dep, '@loader_path/' + child])
        edited = subprocess.run(changes + [str(target)], capture_output=True, text=True)
        if edited.returncode:
            raise RuntimeError(edited.stderr)
        subprocess.run(['codesign', '--force', '--sign', '-', str(target)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        records.append({'file': name, 'input': str(owner), 'input_sha256': hashlib.sha256(owner.read_bytes()).hexdigest()})
        for parent in owner.parents:
            if (parent / 'INSTALL_RECEIPT.json').is_file():
                bottles.add(parent)
                break
    (output / 'PROVENANCE.json').write_text(json.dumps(records, indent=2) + '\n')
    notices = output / 'notices'
    notices.mkdir()
    for bottle in sorted(bottles):
        dest = notices / (bottle.parent.name + '-' + bottle.name)
        dest.mkdir()
        for pattern in ('LICENSE*', 'COPYING*', 'Copyright', 'INSTALL_RECEIPT.json', 'sbom.spdx.json', '.brew/*.rb'):
            for material in bottle.glob(pattern):
                if material.is_file():
                    shutil.copyfile(material, dest / material.name)
    # Match exact installed source versions, never current unrelated upstream releases.
    manifest = []
    cache = args.source_cache.resolve()
    for bottle in sorted(bottles):
        label = bottle.parent.name + '-' + bottle.name
        archives = list(cache.glob(label + '/*')) if cache.is_dir() else []
        archives = [p for p in archives if p.is_file() and not p.name.endswith('.partial')]
        if args.require_source_index:
            verify_source_index(cache / label)
        if not archives:
            manifest.append(f'MISSING | {label} | supply corresponding source and Homebrew patches in {cache / label}')
        else:
            dest = output / 'source' / label
            dest.mkdir(parents=True)
            for archive in archives:
                shutil.copyfile(archive, dest / archive.name)
                manifest.append(f'{label} | {archive.name} | {hashlib.sha256(archive.read_bytes()).hexdigest()}')
    if not bottles:
        manifest.append('MISSING | custom runtime | supply source and license provenance for this closure')
    (output / 'SOURCE-MANIFEST.txt').write_text('\n'.join(manifest) + '\n')
    (output / 'PLAYBACK-NOTICE.txt').write_text(
        'Slicer macOS playback runtime\n\n'
        'libmpv and its private dependencies retain their respective licenses.\n'
        'Homebrew receipts, build formulas, SBOMs and installed notices are in notices/.\n'
        'Source materials are recorded in SOURCE-MANIFEST.txt; MISSING entries mark\n'
        'a development bundle which is not a source-complete release.\n'
        'The separate static export FFmpeg keeps its own source and license boundary.\n')
    check(output, args.arch)
    if any(row.startswith('MISSING') for row in manifest) and not args.allow_missing_sources:
        raise RuntimeError(f'corresponding sources missing; see {output / "SOURCE-MANIFEST.txt"}; --allow-missing-sources is development-only')
    print(f'Playback bundle: {output} ({len(inputs)} dylibs)')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--libmpv', type=Path, help='explicit libmpv.2.dylib input')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--arch', choices=('arm64', 'x86_64'), default=os.uname().machine)
    parser.add_argument('--source-cache', type=Path, default=Path('packaging/playback-source/macos'))
    parser.add_argument('--allow-missing-sources', action='store_true', help='development-only runtime')
    parser.add_argument('--require-source-index', action='store_true', help='verify complete collector SHA256SUMS indexes before bundling')
    parser.add_argument('--check', type=Path, help='verify a relocated, signed dylib closure')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('run this tool on macOS')
    if args.check:
        check(args.check, args.arch)
    elif not args.libmpv or not args.output:
        parser.error('--libmpv and --output are required')
    else:
        bundle(args)


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f'error: {error}')
