# Workload backend API

`ORQOS_BACKENDS=workload` runs **pods of containers as owned systemd units**, using rootful Podman and its Quadlet
generator. It is the execution layer for [Rezn](https://github.com/rezn-project/rezn)'s IR v2 programs (Rezn ADR
0001). This first version covers backend selection and host checks (#16), owned unit lifecycle (#17) and pods
with a shared network namespace and hostname (#18), owned pod networks with DNS aliases (part of #25), container
identity and hardening (#20), scratch volumes (#21), owned volumes (#22), secret files and environment
variables (#24), startup, readiness and liveness probes (#26), init containers and sidecars (#19), and cgroup
limits and requests. Image pulls, live network membership changes, fuller observation, usage accounting and reboot
recovery follow in #23–#32.

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
| `GET /workload/capabilities` | Host requirements and what was found; `problems` is empty when all are met. Includes `capacity`. |
| `GET /workload/capacity` | The host's `cpu_millis` (online CPUs × 1000) and `memory_bytes` (`MemTotal`), for clients' admission checks. |
| `GET /workload/pods` | Every pod with its observed state. |
| `PUT /workload/pods/{id}` | Apply a pod spec: create and start, start an unchanged stopped pod, or replace a changed pod in one go (stop, new units, start). Re-applying an unchanged running pod changes nothing. |
| `GET /workload/pods/{id}` | One pod. |
| `POST /workload/pods/{id}/stop` | Stop the pod gracefully; it stays registered. |
| `POST /workload/pods/{id}/start` | Start a stopped pod. |
| `DELETE /workload/pods/{id}` | Stop, remove its units, confirm Podman has no pod left, delete the record; then remove its network if no other pod names it. |
| `PUT /workload/volumes/{name}` | Create an owned volume, or update its spec (`group`, `labels`). |
| `GET /workload/volumes` | Owned volumes: `name`, `podman_name`, `exists`, `spec`, the `pods` that mount them, timestamps. |
| `DELETE /workload/volumes/{name}` | Delete the volume and its data; refused (409) while any pod mounts it. |
| `PUT /workload/secrets/{name}` | Create an owned secret (`data`: values by key, base64), or update its `labels`. Data is immutable: different data for an existing name is refused (409). |
| `GET /workload/secrets` | Owned secrets: `name`, `keys`, `labels`, the `pods` using them, timestamps. **Never values.** |
| `DELETE /workload/secrets/{name}` | Delete the secret; refused (409) while any pod uses it. |
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
    {"name": "tmp", "scratch": {"medium": "memory", "bytes": 67108864}},
    {"name": "tls", "secret": {"name": "shop-tls", "mode": 288, "group": 2000}}
  ],
  "containers": [
    {"name": "web", "image": "docker.io/library/nginx:1.27-alpine"},
    {"name": "worker", "image": "registry.example.com/shop/worker:1.4",
     "command": ["/app/worker"], "args": ["--mode", "batch"], "env": {"LEVEL": "info"},
     "user": 1000, "group": 1000, "read_only_root": true, "no_new_privileges": true, "drop_all_capabilities": true,
     "secret_env": {"DB_PASSWORD": {"secret": "shop-db", "key": "password"}},
     "mounts": [{"volume": "state", "path": "/var/lib/worker", "sub_path": "v1"}, {"volume": "tmp", "path": "/tmp"},
                {"volume": "tls", "path": "/etc/worker/tls"}]}
  ]
}
```

| Field | Meaning |
| --- | --- |
| `hostname` | Hostname inside every container (DNS label). |
| `init`, `sidecars` | Optional lists of containers (same fields as `containers`; at most 32 containers in all, names unique across the three lists), below. |
| `containers[].name` | DNS label of at most 63 characters, unique in the pod. The container is named `orqos-<id>-<name>`. |
| `containers[].image` | Image reference, pulled when missing. |
| `containers[].command` | Replaces the image's entrypoint and default command (Kubernetes `command`). |
| `containers[].args` | Replaces the image's default command (Kubernetes `args`). Without `command`, empty and lone `;` arguments are rejected: set `command` to pass them. |
| `containers[].env` | Variables passed literally (no `$` or `%` expansion). |
| `containers[].secret_env` | Variables from owned secrets, `{"VAR": {"secret", "key"}}`, passed literally like `env` (names disjoint from it). The value must be UTF-8 without control characters; otherwise the apply is refused (422). |
| `containers[].user`, `containers[].group` | UID and GID inside the pod's user namespace (at most 65535); `group` needs `user`. Without them the image's user runs. |
| `containers[].read_only_root` | Read-only root filesystem (Podman keeps tmpfs on `/dev`, `/run`, `/tmp`, `/var/tmp`; a mount over one replaces it). |
| `containers[].no_new_privileges` | `no-new-privileges`: setuid binaries and file capabilities grant nothing. |
| `containers[].drop_all_capabilities` | Drops every capability. Podman's default seccomp profile always applies. |
| `containers[].startup`, `.readiness`, `.liveness` | Optional probes, below. |
| `containers[].resources` | Optional `cpu_limit_millis`, `cpu_request_millis`, `memory_limit_bytes`, `memory_request_bytes` (CPU 1–1000000, memory 6 MiB–1 TiB, requests at most limits), applied to the container's cgroup: a memory limit sets `memory.max` with no swap (`memory.swap.max=0`), a memory request `memory.low`, a CPU limit `cpu.max` (capped at the host's CPUs, which Podman requires), a CPU request the CPU weight (shares = millis × 1024 / 1000, at least 2). A process past its memory limit is OOM-killed and the restart policy applies. |
| `containers[].mounts` | Up to 32 `{volume, path, read_only?, sub_path?}`: a pod volume at an absolute `path` (letters, digits, `/ . _ - + @`, no `..`), optionally a relative `sub_path` inside it. |
| `supplementary_groups` | Up to 16 extra GIDs of every container (at most 65535). The first owns the pod's scratch volumes. |
| `volumes` | Up to 32 `{name, volume}` (an owned volume, which must exist), `{name, secret: {name, items?, mode?, group?}}` (files from an owned secret, below) or `{name, scratch: {medium, bytes}}`. `memory` scratch is a tmpfs of `bytes`; `disk` scratch is a local volume (`bytes` advisory). Scratch volumes are empty when the pod is created or replaced, shared by its containers, kept across container restarts and removed with the pod. Their root is `root:<first supplementary group>` 2775, or 0777 without one. |
| `volumes[].secret` | Files from an owned secret, mounted **read-only** whatever the mount says, replacing what the image has at the mount path. `items` (`[{key, path}]`, relative paths without empty, `.` or `..` components) selects keys and where they go; without it every key becomes a file of its own name. `mode` (default 0644, at most 0777) applies to every file; directories get `r-x` wherever files get `r` (0440 files sit in 0550 directories). Files and directories belong to root and `group` (or root). A mount's `sub_path` must name a projected file or directory. |
| `restart` | `always` (default), `on_failure` or `never`, for every container. Restarts back off from 1 s, doubling up to 5 minutes. |
| `grace_seconds` | SIGTERM-to-SIGKILL time on stop (default 30, at most 3600). |
| `labels` | Up to 32 labels set on the Podman pod and every container, for the client's own bookkeeping (Rezn records its ownership here). Keys: lowercase letters, digits, `.`, `/` and `-`, at most 128; the `dev.orqos.` prefix is reserved. Values: letters, digits and `. _ : / @ + -`, at most 256. They are part of the spec: changing them replaces the pod. Orqos's own ownership checks never rely on them. |
| `network` | Optional. Without it the pod uses Podman's default network, which has no DNS. `name` (DNS label, at most 40 characters) selects the owned bridge network `orqos-net-<name>`; `aliases` (at most 64 distinct lowercase DNS names, each dot-separated labels of at most 253 characters, kept in order) resolve to the pod for every other pod on that network. Several pods may share an alias, which then resolves to all of their addresses; names resolve only while the pod runs. Pods on different networks cannot resolve or reach each other: networks are created with netavark's `isolate=strict`, which drops traffic between an owned network and every other bridge network, Podman's default network included, while egress to the outside keeps working. Part of the spec: changing the network or its aliases replaces the pod. |

Control characters are rejected everywhere. Unknown fields are rejected.

### Init containers and sidecars

Every container is its own systemd unit in the pod, ordered by systemd:

- `init` containers are one-shot units (`Type=oneshot`, `RemainAfterExit=yes`) that run **in order**, each requiring
  the previous one. One that fails is retried in place (`Restart=on-failure` with backoff, `RestartMode=direct`, so
  the units waiting on it keep waiting) unless the pod's `restart` is `never`, in which case the pod start fails.
  Init containers take no probes. Once done they read `active` / `exited`.
- `sidecars` start after the init containers and are always restarted.
- `containers` start after the init containers and the sidecars (`After=`/`Wants=`, so a sidecar crash does not
  stop them, and on stop they go first). When a sidecar has a `startup` probe, `containers` wait at an
  `ExecStartPre` gate until **every** such probe has passed: Orqos then creates
  `/run/orqos/workload/<id>/sidecars-started` (removed when the pod stops or is replaced).
- Scratch volumes are pod-level, so init containers, sidecars and containers share them.

### Probes

A probe is exactly one of `{"exec": [argv]}` or `{"http": {"path", "port", "scheme": "http" | "https"}}`, with
`period_seconds` (1–3600), `timeout_seconds` (1–300; it may exceed the period, and the next run then waits) and
`failure_threshold` (1–1000), all required:

```json
"readiness": {"http": {"path": "/health/ready", "port": 8080, "scheme": "https"},
              "period_seconds": 10, "timeout_seconds": 3, "failure_threshold": 3}
```

Orqos runs them itself (not as Podman healthchecks, which allow one check per container and run inside it):

- `exec` runs with `podman exec` as the container's user; exit status 0 succeeds. The process gets
  `ORQOS_PROBE=<random token>` in its environment; on timeout Orqos kills every process carrying the token, since a
  timed-out `podman exec` would leave it running in the container.
- `http` sends `GET <path>` to `127.0.0.1:<port>` **from outside the container, inside the pod's network
  namespace** (a short-lived thread enters it with `setns`), so images need no HTTP client. Statuses 200–399
  succeed; `https` does not verify the certificate.
- Until the startup probe succeeds, no other probe runs and the container is not `started`. Readiness turns false
  after `failure_threshold` consecutive failures and true after one success; it never restarts anything. Startup
  and liveness reaching their threshold stop the container (`podman stop -t <grace_seconds>`), so the pod's
  `restart` policy decides what follows (`never` leaves it down).
- A new container instance starts over: not started, not ready. Probes resume when Orqos restarts.

### Observed state

Each pod reports `desired` (`running` or `stopped`), a summary `state` (`running`, `starting`, `restarting`,
`degraded`, `failed` or `stopped`), `ready` (it runs and every sidecar and container is ready), the pod unit and
every container unit (`kind`: `init`, `sidecar` or `container`; `active_state`, `sub_state`, `result`,
`restarts`), with each container's `started`, `ready`,
`probe_restarts` and `last_probe_failure` (without probes, a running container is started and ready), its spec and
the last operation `error`.

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
- **Secrets** are records only (`/var/lib/orqos/workload/secrets/<name>.json`, 0600 in a root-only directory):
  there is no Podman object. Their values are stored **unencrypted**, readable only by root, as Kubernetes keeps
  Secrets in etcd by default. When a pod is created or replaced (its units already stopped), Orqos writes each
  secret volume afresh under `/var/lib/orqos/workload/files/<id>/<volume>/` (root-only parents) and bind-mounts it
  `ro,idmap`, so the container sees root and the group exactly as on disk whatever range its namespace gets.
  Secret environment values go into the container's `EnvironmentFile=` (0600, in the root-only unit directory),
  never into the spec. Because secret data is immutable, a client changes a value by creating a new secret and
  applying pods that name it, which replaces exactly those pods. The files are removed with the pod.
- Stopping Orqos leaves pods running; on start it reports them as they are. Every 10 s it logs pods that should
  run but whose units failed or vanished.
- After a host reboot pods are stopped until started again (`POST .../start` or a re-apply); automatic recovery
  is #28.

## Checks

`cargo test` covers spec validation, network bookkeeping, unit rendering (golden files in `tests/golden/workload/`, accepted by the
Quadlet generator), ownership rules, backend selection and route mounting. `sudo python3
scripts/workload_smoke.py --orqos target/debug/orqos` runs the live lifecycle against real systemd and Podman, including network
aliases, isolation, volumes, secret files, probes, init containers and sidecars, resources, and cleanup; the
*Workload backend* workflow runs it on Ubuntu 26.04 for every push and pull request.
