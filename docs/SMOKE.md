# Local Smoke Checks

Prerequisites: Python 3, a built Orqos binary, Docker CLI and/or kubectl, and reachable local backends. Kubernetes requires permission to create/delete a disposable namespace and watch pods. The script requires a loopback Kubernetes endpoint and Unix Docker socket.

```bash
cargo build --locked
docker --context desktop-linux pull alpine:3.22
python3 scripts/smoke.py --backend both \
  --docker-context desktop-linux --kube-context docker-desktop
```

For individual backends:

```bash
python3 scripts/smoke.py --backend docker --docker-context desktop-linux
python3 scripts/smoke.py --backend kube --kube-context docker-desktop
```

Contexts are explicit; replace these names for another local setup. The script copies the chosen kubeconfig into a temporary private directory without changing global context selections. It uses Alpine 3.22, unique `orqos-smoke-*` fixtures, random loopback HTTP ports, and removes its container, namespace, kubeconfig and daemon processes even after failures. The pulled image remains cached.

Checks cover backend modes and unreachable startup, `/healthz`, lifecycle/filtering, missing Docker images and name conflicts, stdout/stderr and nonzero exits, large stderr, WebSocket command encoding, exact file bytes, empty/binary files, overwrite rules, metadata failures, path restrictions and deletion events. Exit zero and `PASS all requested MVP smoke checks` indicate success; failures raise an assertion with the failing request.

The combined suite passed locally on 2026-10-07 with Docker Desktop Engine 29.8.2, Kubernetes 1.36.4 and kubectl 1.36.5. CI runs the Docker-only variant against context `default`; Kubernetes validation remains local.
