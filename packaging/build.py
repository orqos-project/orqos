#!/usr/bin/python3
"""Package a natively built Orqos binary and its execution assets."""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def build(tag, arch, binary, output):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[a-zA-Z0-9.]+)?', tag):
        raise ValueError('Explicit semver release tag required')
    metadata = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    if metadata['package']['version'] != tag[1:]:
        raise ValueError('Cargo package version differs from tag')
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
    if next(x['version'] for x in lock['package'] if x['name'] == 'orqos') != tag[1:]:
        raise ValueError('Cargo lock version differs from tag')
    if arch not in ('amd64', 'arm64'):
        raise ValueError('Unsupported architecture')
    header = binary.read_bytes()[:20]
    if header[:6] != b'\x7fELF\x02\x01' or int.from_bytes(header[18:20], 'little') != {'amd64': 62, 'arm64': 183}[arch]:
        raise ValueError('Binary architecture differs from package architecture')
    ver = tag[1:].replace('-', '~')
    stage = output / f'stage-{arch}'
    output.mkdir(parents=True, exist_ok=True)
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True, exist_ok=True)
    stage.chmod(0o755)
    for target in ('usr/bin', 'usr/lib/orqos', 'usr/lib/systemd/system', 'DEBIAN'):
        (stage / target).mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, stage / 'usr/bin/orqos')
    for source in ('host.py', 'files.py', 'workspace.py'):
        shutil.copy2(ROOT / 'host' / source, stage / 'usr/lib/orqos' / source)
    shutil.copy2(ROOT / 'packaging/host-admin.py', stage / 'usr/lib/orqos/admin.py')
    wrapper = stage / 'usr/bin/orqos-host-admin'
    wrapper.write_text('#!/bin/sh\nexec /usr/bin/python3 /usr/lib/orqos/admin.py "$@"\n')
    wrapper.chmod(0o755)
    shutil.copy2(ROOT / 'packaging/orqos-host.service', stage / 'usr/lib/systemd/system/orqos-host.service')
    for name in ('preinst', 'postinst', 'prerm', 'postrm'):
        shutil.copy2(ROOT / 'packaging' / name, stage / 'DEBIAN' / name)
        (stage / 'DEBIAN' / name).chmod(0o755)
    (stage / 'DEBIAN/control').write_text(
        f'Package: orqos-host\nVersion: {ver}\nArchitecture: {arch}\n'
        'Maintainer: Orqos <support@rawpair.com>\nSection: devel\nPriority: optional\n'
        'Depends: python3, systemd, iproute2, nftables, tmux, ttyd (>= 1.7), util-linux, '
        'passwd, procps, libc6 (>= 2.39), libssl3t64, libgcc-s1, ca-certificates\n'
        'Description: Orqos native workspace control and execution assets\n')
    for path in stage.rglob('*'):
        if not path.is_symlink():
            path.chmod(path.stat().st_mode & ~0o022)
    environment = os.environ.copy()
    environment.setdefault('SOURCE_DATE_EPOCH', subprocess.check_output(['git', '-C', str(ROOT), 'log', '-1', '--format=%ct'], text=True).strip())
    subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(stage),
                    str(output / f'orqos-host_{ver}_{arch}.deb')], check=True, env=environment)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--tag', required=True)
    p.add_argument('--arch', required=True)
    p.add_argument('--binary', type=Path, default=ROOT / 'target/release/orqos')
    p.add_argument('--output', type=Path, default=ROOT / 'dist')
    a = p.parse_args()
    build(a.tag, a.arch, a.binary, a.output)
