#!/usr/bin/env python3
"""Collect exact corresponding sources for an inventoried native playback closure."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from urllib.parse import urlparse


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def mirror_archive(url, target):
    # SVT's canonical GitHub mirror carries the same Git objects. Recreate
    # GitLab's archive, and still require the installed formula's exact hash.
    prefix = 'https://gitlab.com/AOMediaCodec/SVT-AV1/-/archive/'
    if not url.startswith(prefix) or not url.endswith('.tar.bz2'):
        return False
    import bz2
    tag, filename = url.removeprefix(prefix).split('/', 1)
    mirror = 'https://github.com/AOMediaCodec/SVT-AV1.git'
    with tempfile.TemporaryDirectory() as temporary:
        subprocess.run(['git', 'init', '-q', temporary], check=True)
        subprocess.run(['git', '-C', temporary, 'fetch', '--depth=1', mirror, 'refs/tags/' + tag], check=True)
        archive = subprocess.check_output(['git', '-C', temporary, 'archive', '--format=tar',
            '--prefix=' + filename.removesuffix('.tar.bz2') + '/', 'FETCH_HEAD'])
        target.write_bytes(bz2.compress(archive))
    return True


def fetch(record, directory):
    url = record['url']
    revision = record.get('specs', {}).get('revision')
    expected = record.get('checksum', '')
    if revision:
        name = record['name'] + '-' + revision + '.tar.gz'
        target = directory / name
        # Cached Git snapshots are reused only with their original provenance hash.
        metadata = directory / (name + '.json')
        if target.exists() and metadata.exists():
            saved = json.loads(metadata.read_text())
            if saved.get('url') == url and saved.get('revision') == revision and saved.get('sha256') == sha(target):
                return
        with tempfile.TemporaryDirectory() as temporary:
            tree = Path(temporary) / 'source'
            subprocess.run(['git', 'init', '-q', str(tree)], check=True)
            subprocess.run(['git', '-C', str(tree), 'fetch', '--depth=1', url, revision], check=True)
            subprocess.run(['git', '-C', str(tree), 'checkout', '-q', 'FETCH_HEAD'], check=True)
            if run('git', '-C', str(tree), 'rev-parse', 'HEAD') != revision:
                raise RuntimeError('source Git revision mismatch')
            subprocess.run(['git', '-C', str(tree), 'submodule', 'update', '--init', '--recursive', '--depth=1'], check=True)
            snapshot = Path(temporary) / record['name']
            shutil.copytree(tree, snapshot, ignore=shutil.ignore_patterns('.git'))
            archive = shutil.make_archive(str(Path(temporary) / 'archive'), 'gztar', temporary, record['name'])
            shutil.move(archive, target)
        metadata.write_text(json.dumps({'url': url, 'revision': revision, 'sha256': sha(target)}, indent=2) + '\n')
    else:
        if not expected:
            raise RuntimeError('no exact source checksum for ' + url)
        name = record['name'] + '-' + Path(urlparse(url).path).name
        target = directory / name
        if not target.exists() or sha(target) != expected:
            partial = directory / (name + '.partial')
            try:
                subprocess.run(['curl', '-fL', '--retry', '3', '--retry-all-errors', '--connect-timeout', '20', '--max-time', '600', '-o', str(partial), url], check=True)
            except subprocess.CalledProcessError:
                if not mirror_archive(url, partial):
                    raise
            if sha(partial) != expected:
                raise RuntimeError('source SHA-256 mismatch: ' + url)
            partial.replace(target)


def macos(bundle, cache):
    bottles = set()
    for row in json.loads((bundle / 'PROVENANCE.json').read_text()):
        for parent in Path(row['input']).parents:
            if (parent / 'INSTALL_RECEIPT.json').is_file():
                bottles.add(parent)
                break
        else:
            raise RuntimeError('source input has no installed Homebrew provenance: ' + row['input'])
    formulas = [next(bottle.glob('.brew/*.rb')) for bottle in sorted(bottles)]
    # Read the saved installed formulas, never the current unrelated formula version.
    ruby = '''puts JSON.generate(ARGV.map do |path|
      f=Formulary.factory(Pathname.new(path)); s=f.stable
      {url:s.url, checksum:s.checksum.to_s, specs:s.specs,
       resources:s.resources.values.map {|r| {name:r.name,url:r.url,checksum:r.checksum.to_s,specs:r.specs}}}
    end)'''
    env = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE='1', HOMEBREW_DEVELOPER='1')
    records = json.loads(run('brew', 'ruby', '-rformulary', '-rjson', '-e', ruby, *map(str, formulas), env=env))
    for bottle, formula, main in zip(sorted(bottles), formulas, records):
        label = bottle.parent.name + '-' + bottle.name
        directory = cache / label
        directory.mkdir(parents=True, exist_ok=True)
        print('Collecting sources:', label, flush=True)
        main['name'] = bottle.parent.name
        resources = main.pop('resources')
        fetch(main, directory)
        for resource in resources:
            fetch(resource, directory)
        sbom = bottle / 'sbom.spdx.json'
        for package in json.loads(sbom.read_text())['packages']:
            if package['SPDXID'].startswith('SPDXRef-Patch-'):
                checksum = next(c['checksumValue'] for c in package['checksums'] if c['algorithm'] == 'SHA256')
                fetch({'name': package['SPDXID'], 'url': package['downloadLocation'], 'checksum': checksum}, directory)
        for material in [formula, sbom, bottle / 'INSTALL_RECEIPT.json']:
            shutil.copyfile(material, directory / material.name)
        # Include hashes for all sources, patches, recipes and resources.
        (directory / 'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.name}\n' for p in sorted(directory.iterdir()) if p.is_file() and p.name != 'SHA256SUMS'))


def linux(bundle, cache):
    cache.mkdir(parents=True, exist_ok=True)
    sources = set()
    for line in (bundle / 'SOURCE-MANIFEST.txt').read_text().splitlines():
        if line.startswith('#') or not line.strip():
            continue
        fields = [s.strip() for s in line.split('|')]
        if len(fields) == 8:
            sources.add((fields[3], fields[4]))
    if not sources:
        raise RuntimeError('no distro source packages in playback inventory')
    for package, version in sorted(sources):
        print('Collecting sources:', package, version, flush=True)
        subprocess.run(['apt-get', 'source', '--download-only', '--only-source', package + '=' + version], cwd=cache, check=True)
    # The strict bundler verifies .dsc versions and every archive size/hash next.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['macos', 'linux'])
    parser.add_argument('bundle', type=Path)
    parser.add_argument('cache', type=Path)
    args = parser.parse_args()
    globals()[args.platform](args.bundle.resolve(), args.cache.resolve())


if __name__ == '__main__':
    main()
