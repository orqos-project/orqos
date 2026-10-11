#!/usr/bin/python3
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--tag', required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
version = a.tag.removeprefix('v').replace('-', '~')
for arch in ('amd64', 'arm64'):
    path = a.output / f'orqos-host_{version}_{arch}.deb'
    for field, expected in [('Package', 'orqos-host'), ('Version', version), ('Architecture', arch)]:
        if subprocess.check_output(['dpkg-deb', '-f', str(path), field], text=True).strip() != expected:
            raise ValueError(f'{path}: inconsistent {field}')
    target = 'x86_64' if arch == 'amd64' else 'aarch64'
    if not (a.output / f'orqos-{target}-unknown-linux-gnu').is_file():
        raise ValueError('Missing standalone binary: ' + target)
artifacts = sorted([*a.output.glob('*.deb'), *a.output.glob('orqos-*-unknown-linux-gnu')])
(a.output / 'manifest.json').write_text(json.dumps({'schema': 1, 'tag': a.tag, 'version': version,
                                                   'ubuntu': '24.04', 'ubuntu_versions': ['24.04', '26.04'],
                                                   'architectures': ['amd64', 'arm64'],
                                                   'artifacts': [{'name': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()} for path in artifacts]}, indent=2) + '\n')
files = sorted([*artifacts, a.output / 'manifest.json'])
(a.output / 'SHA256SUMS').write_text(''.join(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.name + '\n' for path in files))
