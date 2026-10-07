# Orqos MVP Assessment

Updated: 2026-10-07, working tree on `feature/split-between-docker-and-kube` (base commit `3996b4a`).

The original audit confirmed corrupt file reads and unusable WebSocket command queries on both backends, Docker upload/permission bugs, and Kubernetes exit-code loss. These defects are fixed and covered by regression and live checks.

## Verified Results

| Check | Result |
| --- | --- |
| Locked build, formatting, tests, strict Clippy | Pass; 15 regression tests. |
| Docker Desktop Engine 29.8.2 | Create/list/filter/exec/write/read/stop/remove pass. |
| Kubernetes 1.36.4; kubectl 1.36.5 | Create/list/filter/exec/write/read/delete pass. |
| File transfer, both backends | Exact text, empty, Unicode/NUL and binary reads; overwrite conflicts; ownership/mode application; `chmod`/`chown` failures; directory/symlink/path rejection. |
| Exec, both backends | Buffered stdout/stderr, 128 KiB stderr without stalls, exit 7, WebSocket JSON command queries and exit sentinel, invalid-command rejection. |
| Backend modes and health | Docker-only, Kubernetes-only, combined, either backend unavailable, and no reachable backend behave correctly. |
| Events | Docker events and Kubernetes `APPLIED`/`DELETED` events delivered. |
| OpenAPI | Health, metrics and both WebSocket exec routes included. |

The [smoke script](SMOKE.md) used `desktop-linux` and `docker-desktop`, a uniquely named container and namespace, and temporary daemon processes. All fixtures and processes were cleaned up; user workloads and global contexts were preserved.

## CI and Remaining Limits

Push/PR checks now build, format, test, lint with warnings denied, and run Docker smoke checks. Release artifacts remain configured. Hosted CI has not run for these uncommitted changes; local equivalents passed.

Podman has not been tested live. Kubernetes metrics and pod logs remain deferred. File transfer buffers data in memory; writes and metadata changes are separate steps, and overwrite checks are not atomic. Kubernetes targets require standard file utilities. Event delivery is best effort across disconnects. Configuration/client-construction failures require restart; live clients are rechecked.

Both local Docker engines can remain running. Orqos selects an explicit Unix endpoint; the CLI's active Docker context is not inferred. See [configuration](../README.md#local-development).
