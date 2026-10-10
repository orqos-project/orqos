#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID != 0 ]]; then echo 'Run as root after building target/release/orqos.' >&2; exit 1; fi
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
install -d -o root -g root -m 0755 /etc/orqos /usr/lib/orqos
install -d -o root -g root -m 0700 /var/lib/orqos /var/lib/orqos/host
install -o root -g root -m 0755 target/release/orqos /usr/local/bin/orqos
install -o root -g root -m 0644 host/host.py host/files.py host/workspace.py /usr/lib/orqos/
install -o root -g root -m 0644 packaging/orqos-host.service /etc/systemd/system/
if [[ ! -e /etc/orqos/host.json ]]; then
  install -o root -g root -m 0600 packaging/host.example.json /etc/orqos/host.json
fi
if [[ ! -e /etc/orqos/host.env ]]; then
  python3 - <<'PY'
import os, secrets
fd = os.open('/etc/orqos/host.env', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as stream:
    stream.write('ORQOS_HOST_TOKEN=' + secrets.token_hex(32) + '\n')
PY
fi
# Routing is required. Change only this explicit sysctl, never firewall defaults.
install -d -m 0755 /etc/sysctl.d
printf '%s\n' 'net.ipv4.ip_forward=1' > /etc/sysctl.d/70-orqos-host.conf
sysctl -p /etc/sysctl.d/70-orqos-host.conf
systemctl daemon-reload
printf '%s\n' 'Installed. Provision inventory accounts, review /etc/orqos/host.json, then:' \
  '  systemctl enable --now orqos-host' \
  'See docs/HOST_API.md and RawPair docs/NATIVE_SETUP.md.'
