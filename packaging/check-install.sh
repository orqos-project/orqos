#!/usr/bin/env bash
# Destructive package acceptance on disposable GitHub-hosted runners only.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true ]] || { echo 'Disposable CI runner required' >&2; exit 1; }
dist=$(realpath -- "$1")
sudo apt-get -s install "$dist"/*.deb
sudo apt-get install -y "$dist"/*.deb
test ! -e /etc/orqos/host.env
sudo orqos-host-admin init
sudo touch /etc/skel/orqos-skeleton-must-not-copy
sudo orqos-host-admin provision project-a
sudo test ! -e /srv/rawpair/project-a/orqos-skeleton-must-not-copy
sudo orqos-host-admin provision project-a
sudo orqos-host-admin provision project-b
sudo orqos-host-admin validate
sudo systemctl enable --now orqos-host
sudo -H env GITHUB_ACTIONS=true python3 packaging/runtime-smoke.py
sudo apt-get install -y --reinstall "$dist"/*.deb
sudo orqos-host-admin validate
sudo apt-get remove -y orqos-host
sudo test -f /etc/orqos/host.json
sudo test -f /var/lib/orqos/provisioning.json
sudo test -d /srv/rawpair/project-a/project
