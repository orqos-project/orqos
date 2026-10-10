#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID != 0 || $# != 1 || ! $1 =~ ^[a-z][a-z0-9-]{0,24}$ ]]; then
  echo 'Usage: sudo packaging/provision-workspace.sh project-a' >&2; exit 1
fi
slot=$1
account="rp-$slot"
project_home="/srv/rawpair/$slot"
if getent passwd "$account" >/dev/null; then
  echo "Account $account already exists; inspect ownership before proceeding." >&2; exit 1
fi
install -d -o root -g root -m 0755 /srv/rawpair
useradd --create-home --home-dir "$project_home" --shell /usr/sbin/nologin --expiredate 1970-01-02 --user-group "$account"
chmod 0700 "$project_home"
install -d -o "$account" -g "$account" -m 0700 "$project_home/project" "$project_home/.local/bin"
# No password, sudo, SSH keys, shared groups, or host login is granted.
passwd --lock "$account" >/dev/null
printf 'Provisioned %s. Add its mapping with a unique network index to /etc/orqos/host.json.\n' "$account"
