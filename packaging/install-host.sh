#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID != 0 || $# -gt 1 ]]; then echo 'Usage: sudo packaging/install-host.sh [PRIVATE_SUBNET/16]' >&2; exit 1; fi
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
if [[ -f /var/lib/orqos/package-owner ]]; then echo 'Use the package tools: sudo orqos-host-admin init' >&2; exit 1; fi
if [[ ! -f /var/lib/orqos/source-owner ]]; then
  for path in /usr/bin/orqos /usr/local/bin/orqos /etc/orqos/host.json /etc/systemd/system/orqos-host.service; do
    if [[ -e $path || -L $path ]]; then echo "Existing installation at $path; see docs/PACKAGES.md." >&2; exit 1; fi
  done
fi
install -d -o root -g root -m 0755 /etc/orqos /usr/lib/orqos
install -d -o root -g root -m 0700 /var/lib/orqos /var/lib/orqos/host
printf '%s\n' orqos-host-v1 > /var/lib/orqos/source-owner
if systemctl cat orqos-host >/dev/null 2>&1; then systemctl stop orqos-host; fi
install -o root -g root -m 0755 target/release/orqos /usr/bin/orqos
install -o root -g root -m 0644 host/host.py host/files.py host/workspace.py /usr/lib/orqos/
install -o root -g root -m 0644 packaging/host-admin.py /usr/lib/orqos/admin.py
printf '%s\n' '#!/bin/sh' 'exec /usr/bin/python3 /usr/lib/orqos/admin.py "$@"' > /usr/bin/orqos-host-admin
chmod 0755 /usr/bin/orqos-host-admin
install -o root -g root -m 0644 packaging/orqos-host.service /usr/lib/systemd/system/
systemctl daemon-reload
orqos-host-admin init --subnet "${1:-10.231.0.0/16}"
printf '%s\n' 'Provision slots with orqos-host-admin provision NAME, then systemctl enable --now orqos-host.'
