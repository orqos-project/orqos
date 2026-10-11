#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID != 0 || $# != 1 ]]; then echo 'Usage: sudo packaging/provision-workspace.sh NAME' >&2; exit 1; fi
exec /usr/bin/orqos-host-admin provision "$1"
