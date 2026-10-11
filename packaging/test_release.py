import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ReleaseTests(unittest.TestCase):
    def test_native_assets_appear_once_in_manifest_and_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            expected = set()
            for arch, target in [('amd64', 'x86_64'), ('arm64', 'aarch64')]:
                stage = output / ('stage-' + arch)
                (stage / 'DEBIAN').mkdir(parents=True)
                (stage / 'DEBIAN/control').write_text(
                    f'Package: orqos-host\nVersion: 0.2.0\nArchitecture: {arch}\n'
                    'Maintainer: Test <test@example.com>\nDescription: Inert test fixture\n')
                name = f'orqos-host_0.2.0_{arch}.deb'
                subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(stage), str(output / name)],
                               check=True, stdout=subprocess.DEVNULL)
                binary = f'orqos-{target}-unknown-linux-gnu'
                (output / binary).write_bytes(b'inert standalone binary fixture')
                expected.update([name, binary])
            subprocess.run([sys.executable, str(ROOT / 'packaging/release.py'), '--tag', 'v0.2.0', '--output', str(output)], check=True)
            manifest = json.loads((output / 'manifest.json').read_text())
            self.assertEqual(manifest['ubuntu'], '24.04')
            self.assertEqual(manifest['ubuntu_versions'], ['24.04', '26.04'])
            names = [artifact['name'] for artifact in manifest['artifacts']]
            self.assertEqual(len(names), len(expected))
            self.assertEqual(set(names), expected)
            entries = [line.split('  ') for line in (output / 'SHA256SUMS').read_text().splitlines()]
            self.assertEqual(len(entries), len(expected) + 1)
            self.assertEqual({name for _, name in entries}, expected | {'manifest.json'})
            for digest, name in entries:
                self.assertEqual(digest, hashlib.sha256((output / name).read_bytes()).hexdigest())
            for artifact in manifest['artifacts']:
                self.assertEqual(artifact['sha256'], hashlib.sha256((output / artifact['name']).read_bytes()).hexdigest())


if __name__ == '__main__':
    unittest.main()
