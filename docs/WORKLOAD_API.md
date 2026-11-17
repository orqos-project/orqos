# Workload backend API

`ORQOS_BACKENDS=workload` runs **pods of containers as owned systemd units**, using rootful Podman and its Quadlet
generator. It is the execution layer for [Rezn](https://github.com/rezn-project/rezn)'s IR v2 programs (Rezn ADR
0001). This first version covers backend selection and host checks (#16), owned unit lifecycle (#17) and pods
with a shared network namespace and hostname (#18). Init containers and sidecars, hardening, volumes, image
pulls, files, networks, probes, fuller observation and reboot recovery follow in #19–#28.

## Prerequisites

- Linux with systemd **254 or newer** and cgroup v2.
- Podman **5.0 or newer** with its Quadlet system generator (Ubuntu 26.04: `apt install podman`; Ubuntu 24.04's
  Podman 4.9 is not supported).
- Orqos running as **root**.
- Subordinate ID ranges for the `containers` user, from which `UserNS=auto` gives every pod its own range:

  ```sh
  for f in /etc/subuid /etc/subgid; do
    grep -q '^containers:' "$f" || echo 'containers:2147483647:2147483648' | sudo tee -a "$f"
  done
  ```

- `ORQOS_WORKLOAD_TOKEN`: a random secret of at least 32 bytes. Every `/workload` request needs
  `Authorization: Bearer <token>`.
- A loopback `BIND_ADDR`; startup refuses anything else.

Startup checks all of these and stops with a message naming what is missing. `GET /workload/capabilities` and the
`workload` entry of `/healthz` report the same checks later.

`workload` combines with the container backends: `ORQOS_BACKENDS=workload,docker` serves both, so Rezn can run
IR v1 on Docker and IR v2 on workloads during the transition. `host`, `docker`, `kube` and `both` keep their
meaning; `host` cannot be combined.

```sh
sudo ORQOS_BACKENDS=workload ORQOS_WORKLOAD_TOKEN="$(openssl rand -hex 32)" BIND_ADDR=127.0.0.1:9090 orqos
```

## Contract

| Method and path | Effect |
| --- | --- |
| `GET /workload/capabilities` | Host requirements and what was found; `problems` is empty when all are met. |
| `GET /workload/pods` | Every pod with its observed state. |
| `PUT /workload/pods/{id}` | Apply a pod spec: create and start, start an unchanged stopped pod, or replace a changed pod in one go (stop, new units, start). Re-applying an unchanged running pod changes nothing. |
| `GET /workload/pods/{id}` | One pod. |
| `POST /workload/pods/{id}/stop` | Stop the pod gracefully; it stays registered. |
| `POST /workload/pods/{id}/start` | Start a stopped pod. |
| `DELETE /workload/pods/{id}` | Stop, remove its units, confirm Podman has no pod left, delete the record. |

Pod IDs are DNS labels of at most 48 characters. Errors are `{"error": "..."}` with **422** for an invalid spec
(or units Quadlet rejects), **404** for an unknown pod, **409** when units or a Podman pod with the pod's names
exist without its ownership token, **502** when systemd or Podman fails (the message names the command and its
error), and **401** without the bearer token.

### Pod spec

```json
{
  "hostname": "shop-0",
  "restart": "always",
  "grace_seconds": 30,
  "labels": {"example.org/app": "shop"},
  "containers": [
    {"name": "web", "image": "docker.io/library/nginx:1.27-alpine"},
    {"name": "worker", "image": "registry.example.com/shop/worker:1.4",
     "command": ["/app/worker"], "args": ["--mode", "batch"], "env": {"LEVEL": "info"}}
  ]
}
```

| Field | Meaning |
| --- | --- |
| `hostname` | Hostname inside every container (DNS label). |
| `containers[].name` | DNS label of at most 63 characters, unique in the pod. The container is named `orqos-<id>-<name>`. |
| `containers[].image` | Image reference, pulled when missing. |
| `containers[].command` | Replaces the image's entrypoint and default command (Kubernetes `command`). |
| `containers[].args` | Replaces the image's default command (Kubernetes `args`). Without `command`, empty and lone `;` arguments are rejected: set `command` to pass them. |
| `containers[].env` | Variables passed literally (no `$` or `%` expansion). |
| `restart` | `always` (default), `on_failure` or `never`, for every container. Restarts back off from 1 s, doubling up to 5 minutes. |
| `grace_seconds` | SIGTERM-to-SIGKILL time on stop (default 30, at most 3600). |
| `labels` | Up to 32 labels set on the Podman pod and every container, for the client's own bookkeeping (Rezn records its ownership here). Keys: lowercase letters, digits, `.`, `/` and `-`, at most 128; the `dev.orqos.` prefix is reserved. Values: letters, digits and `. _ : / @ + -`, at most 256. They are part of the spec: changing them replaces the pod. Orqos's own ownership checks never rely on them. |

Control characters are rejected everywhere. Unknown fields are rejected.

### Observed state

Each pod reports `desired` (`running` or `stopped`), a summary `state` (`running`, `starting`, `restarting`,
`degraded`, `failed` or `stopped`), the pod unit and every container unit (`active_state`, `sub_state`,
`result`, `restarts`), its spec and the last operation `error`.

## Lifecycle and ownership

- Each pod gets a 128-bit **ownership token**. It appears in the unit directory
  `/etc/containers/systemd/orqos/<token>/` (with an `.orqos-owner` marker), every unit's description, and the
  `dev.orqos.owner` label of the Podman pod and its containers. Before stopping, replacing or removing anything,
  Orqos checks that each unit comes from that directory with that token and that the Podman pod carries that
  label. It refuses to create a pod whose unit or Podman pod names already exist.
- Units are rendered into `/etc/containers/orqos-staging/<token>`, validated with the Quadlet generator's dry run,
  moved into place, and loaded with `systemctl daemon-reload`.
- Generated services are `orqos-<id>-pod.service` and `orqos-<id>-c-<container>.service`. Each pod runs with
  `UserNS=auto`, so container UIDs, including root, map to an unprivileged host range.
- Records live in `/var/lib/orqos/workload/<id>.json` (root-only, written atomically). One Orqos process holds
  the directory's lock.
- Stopping Orqos leaves pods running; on start it reports them as they are. Every 10 s it logs pods that should
  run but whose units failed or vanished.
- After a host reboot pods are stopped until started again (`POST .../start` or a re-apply); automatic recovery
  is #28.

## Checks

`cargo test` covers spec validation, unit rendering (golden files in `tests/golden/workload/`, accepted by the
Quadlet generator), ownership rules, backend selection and route mounting. `sudo python3
scripts/workload_smoke.py --orqos target/debug/orqos` runs the live lifecycle against real systemd and Podman; the
*Workload backend* workflow runs it on Ubuntu 26.04 for every push and pull request.
