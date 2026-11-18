# Workload backend API

`ORQOS_BACKENDS=workload` runs **pods of containers as owned systemd units**, using rootful Podman and its Quadlet
generator. It is the execution layer for [Rezn](https://github.com/rezn-project/rezn)'s IR v2 programs (Rezn ADR
0001). This first version covers backend selection and host checks (#16), owned unit lifecycle (#17) and pods
with a shared network namespace and hostname (#18), owned pod networks with DNS aliases (part of #25), container
identity and hardening (#20), scratch volumes (#21) and owned volumes (#22). Init containers and sidecars, image
pulls, files, live network membership changes, probes, fuller observation and reboot recovery follow in
#19–#28.

## Prerequisites

- Linux with systemd **254 or newer** and cgroup v2.
- Podman **5.0 or newer** with its Quadlet system generator (Ubuntu 26.04: `apt install podman`; Ubuntu 24.04's
  Podman 4.9 is not supported).
- Orqos running as **root**.
- Podman's netavark network backend with aardvark-dns (the default on Ubuntu 26.04), which resolves pod network
  aliases.
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
| `DELETE /workload/pods/{id}` | Stop, remove its units, confirm Podman has no pod left, delete the record; then remove its network if no other pod names it. |
| `PUT /workload/volumes/{name}` | Create an owned volume, or update its spec (`group`, `labels`). |
| `GET /workload/volumes` | Owned volumes: `name`, `podman_name`, `exists`, `spec`, the `pods` that mount them, timestamps. |
| `DELETE /workload/volumes/{name}` | Delete the volume and its data; refused (409) while any pod mounts it. |
| `GET /workload/networks` | Owned pod networks: `name`, `podman_name`, whether the Podman network `exists`, the `pods` that name it and `created_at`. |

Pod IDs are DNS labels of at most 48 characters. Errors are `{"error": "..."}` with **422** for an invalid spec
(or units Quadlet rejects), **404** for an unknown pod, **409** when units or a Podman pod with the pod's names
exist without its ownership token (or a Podman network of the spec's network name exists that Orqos does not
own), **502** when systemd or Podman fails (the message names the command and its
error), and **401** without the bearer token.

### Pod spec

```json
{
  "hostname": "shop-0",
  "restart": "always",
  "grace_seconds": 30,
  "labels": {"example.org/app": "shop"},
  "network": {"name": "shop", "aliases": ["web", "web.shop.svc", "web-0.web"]},
  "supplementary_groups": [2000],
  "volumes": [
    {"name": "state", "volume": "shop-state"},
    {"name": "tmp", "scratch": {"medium": "memory", "bytes": 67108864}}
  ],
  "containers": [
    {"name": "web", "image": "docker.io/library/nginx:1.27-alpine"},
    {"name": "worker", "image": "registry.example.com/shop/worker:1.4",
     "command": ["/app/worker"], "args": ["--mode", "batch"], "env": {"LEVEL": "info"},
     "user": 1000, "group": 1000, "read_only_root": true, "no_new_privileges": true, "drop_all_capabilities": true,
     "mounts": [{"volume": "state", "path": "/var/lib/worker", "sub_path": "v1"}, {"volume": "tmp", "path": "/tmp"}]}
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
| `containers[].user`, `containers[].group` | UID and GID inside the pod's user namespace (at most 65535); `group` needs `user`. Without them the image's user runs. |
| `containers[].read_only_root` | Read-only root filesystem (Podman keeps tmpfs on `/dev`, `/run`, `/tmp`, `/var/tmp`; a mount over one replaces it). |
| `containers[].no_new_privileges` | `no-new-privileges`: setuid binaries and file capabilities grant nothing. |
| `containers[].drop_all_capabilities` | Drops every capability. Podman's default seccomp profile always applies. |
| `containers[].mounts` | Up to 32 `{volume, path, read_only?, sub_path?}`: a pod volume at an absolute `path` (letters, digits, `/ . _ - + @`, no `..`), optionally a relative `sub_path` inside it. |
| `supplementary_groups` | Up to 16 extra GIDs of every container (at most 65535). The first owns the pod's scratch volumes. |
| `volumes` | Up to 32 `{name, volume}` (an owned volume, which must exist) or `{name, scratch: {medium, bytes}}`. `memory` scratch is a tmpfs of `bytes`; `disk` scratch is a local volume (`bytes` advisory). Scratch volumes are empty when the pod is created or replaced, shared by its containers, kept across container restarts and removed with the pod. Their root is `root:<first supplementary group>` 2775, or 0777 without one. |
| `restart` | `always` (default), `on_failure` or `never`, for every container. Restarts back off from 1 s, doubling up to 5 minutes. |
| `grace_seconds` | SIGTERM-to-SIGKILL time on stop (default 30, at most 3600). |
| `labels` | Up to 32 labels set on the Podman pod and every container, for the client's own bookkeeping (Rezn records its ownership here). Keys: lowercase letters, digits, `.`, `/` and `-`, at most 128; the `dev.orqos.` prefix is reserved. Values: letters, digits and `. _ : / @ + -`, at most 256. They are part of the spec: changing them replaces the pod. Orqos's own ownership checks never rely on them. |
| `network` | Optional. Without it the pod uses Podman's default network, which has no DNS. `name` (DNS label, at most 40 characters) selects the owned bridge network `orqos-net-<name>`; `aliases` (at most 64 distinct lowercase DNS names, each dot-separated labels of at most 253 characters, kept in order) resolve to the pod for every other pod on that network. Several pods may share an alias, which then resolves to all of their addresses; names resolve only while the pod runs. Pods on different networks cannot resolve or reach each other: networks are created with netavark's `isolate=strict`, which drops traffic between an owned network and every other bridge network, Podman's default network included, while egress to the outside keeps working. Part of the spec: changing the network or its aliases replaces the pod. |

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
- **Networks** are Podman networks, not units. The first pod that names a network gives it a record
  (`/var/lib/orqos/workload/networks/<name>.json`) with its own token; Orqos then creates `orqos-net-<name>` with
  labels `dev.orqos.owner=<token>` and `dev.orqos.network=<name>`. An existing Podman network of that name without
  a record is refused (409), never adopted, and a recorded network whose owner label differs is refused too. A
  recorded network that vanished is created again on the next apply or start. Networks are created with
  `--opt isolate=strict`; Podman networks cannot be changed afterwards. When no pod names a network any
  more (after a removal, a pod moving to another network, or found by the periodic check), Orqos verifies the
  label, removes the Podman network and then its record.
- **User namespaces and volumes.** Every pod has `UserNS=auto:size=65536`: its own range of 65536 host IDs, so
  container IDs up to 65535 exist, but the range can differ after a restart. Volumes are therefore mounted
  **idmapped**: on disk, files keep the IDs the container sees (UID 1000 is stored as 1000), whatever range the
  pod gets, so data written by one pod is readable by the next.
- **Owned volumes** are Podman volumes `orqos-vol-<name>` with a record
  (`/var/lib/orqos/workload/volumes/<name>.json`) and token, labelled `dev.orqos.owner` and `dev.orqos.volume`.
  They are created with explicit ownership (`o=uid=0,gid=<group>`, which also stops Podman from chowning an empty
  volume to its first user) and a root of `root:<group>` with mode 2775 (setgid, so new files inherit the group),
  or `root:root` 0755 without a group. A later PUT sets the root's group again only if it differs; contents are
  never changed recursively. The client's labels live in the record and may change. An existing Podman volume
  without a record is refused, a recorded volume that vanished is reported (409) rather than recreated empty, and
  a pod can mount only recorded volumes that carry their token. Volumes outlive pods until deleted.
- Stopping Orqos leaves pods running; on start it reports them as they are. Every 10 s it logs pods that should
  run but whose units failed or vanished.
- After a host reboot pods are stopped until started again (`POST .../start` or a re-apply); automatic recovery
  is #28.

## Checks

`cargo test` covers spec validation, network bookkeeping, unit rendering (golden files in `tests/golden/workload/`, accepted by the
Quadlet generator), ownership rules, backend selection and route mounting. `sudo python3
scripts/workload_smoke.py --orqos target/debug/orqos` runs the live lifecycle against real systemd and Podman, including network
aliases, isolation and cleanup; the
*Workload backend* workflow runs it on Ubuntu 26.04 for every push and pull request.
