#!/usr/bin/env python3
"""Assemble an offline AI bundle from pinned, checksum-verified release assets."""
import argparse
import hashlib
import json
import pathlib
import shutil
import tarfile
import tempfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def download(url, checksum, cache):
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / checksum
    if path.exists():
        if digest(path) != checksum:
            raise ValueError(f'Cached artifact checksum mismatch: {path}')
        return path
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as temporary:
        temporary_path = pathlib.Path(temporary.name)
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                shutil.copyfileobj(response, temporary)
            temporary.flush()
            if digest(temporary_path) != checksum:
                raise ValueError('Downloaded artifact checksum mismatch')
            temporary_path.replace(path)
        finally:
            temporary_path.unlink(missing_ok=True)
    return path


def assemble(package, platform, cache):
    manifest = json.loads((ROOT / 'ai/manifest.json').read_text())
    model = manifest['model']
    runtime = manifest['runtime'][platform]
    destination = package / 'ai'
    if destination.exists():
        raise ValueError(f'Refusing to overwrite an existing AI bundle: {destination}')
    model_path = download(model['url'], model['sha256'], cache)
    if model_path.stat().st_size != model['bytes'] or model['bytes'] > 1_500_000_000:
        raise ValueError('Model exceeds its verified size budget')
    archive = download('https://github.com/ggml-org/llama.cpp/releases/download/' +
                       manifest['runtime']['version'] + '/' + runtime['file'], runtime['sha256'], cache)
    package.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=package, prefix='.ai-build-') as temp:
        stage = pathlib.Path(temp)
        with tarfile.open(archive) as tar:
            tar.extractall(stage / 'unpacked', filter='data')
        servers = list((stage / 'unpacked').rglob('llama-server'))
        if len(servers) != 1:
            raise ValueError('Expected exactly one runtime executable')
        bundle = stage / 'ai'
        bundle.mkdir()
        shutil.copytree(servers[0].parent, bundle / 'runtime', symlinks=True)
        shutil.copyfile(model_path, bundle / 'model.gguf')
        shutil.copytree(ROOT / 'ai/licenses', bundle / 'licenses')
        shutil.copyfile(ROOT / 'ai/NOTICE.txt', bundle / 'NOTICE.txt')
        shutil.copyfile(ROOT / 'ai/manifest.json', bundle / 'manifest.json')
        bundle.rename(destination)
    print(f'AI bundle ready: {destination} (model {model["bytes"]:,} bytes)')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', type=pathlib.Path)
    parser.add_argument('--platform', required=True, choices=['macos-arm64', 'ubuntu-x64'])
    parser.add_argument('--cache', type=pathlib.Path, default=pathlib.Path.home() / '.cache/kube-ai-assets')
    args = parser.parse_args()
    assemble(args.package, args.platform, args.cache)
