# Native host backend API

`ORQOS_BACKENDS=host` starts without probing Docker or Kubernetes. `docker`, `kube`, and `both` retain their meanings and existing routes. Native mode is Linux only, requires systemd with cgroup v2, Python 3.10+, Linux openat2, iproute2, nftables, tmux, and ttyd with Unix-socket support. Ubuntu 24.04 is the initial deployment target.

Start with [package or source installation](PACKAGES.md) for the standalone service. RawPair users should use its [paired installer](https://github.com/rawpair/rawpair/blob/main/docs/PACKAGES.md); it initializes Orqos and creates the default slots. [The read-only inventory example](PACKAGES.md#read-your-inventory-through-the-api) verifies authenticated access before you build an integration.

Each slot maps one account to a home/project directory and unique network index. Accounts need their own primary group, no supplementary groups, a nologin shell, locked passwords and an expired account date; the provisioner sets these restrictions. This blocks SSH access and SSH forwarding outside the workspace namespace; systemd launches the terminal shell explicitly without PAM login. Keep inventory and `/etc/orqos/host.env` root owned, mode 0600. Start the initialized service with `sudo systemctl enable --now orqos-host`.

The host daemon binds `127.0.0.1:9080`; non-loopback binding is rejected. Every `/host` endpoint requires `Authorization: Bearer <ORQOS_HOST_TOKEN>`. Give this token only to the unprivileged RawPair service and its local reverse proxy, never to browsers or workspace accounts.

The root daemon exposes a fixed action vocabulary. Launch executables, systemd properties, Unix identities, home/project paths, network address allocation, and DNS come from installed code or root-owned operator inventory. Phoenix does not run privileged commands. Do not expose the Orqos port directly through a public proxy. Container modes retain their existing security and deployment contracts; use the separate host service for this integration.

## Contract

All bodies and responses are JSON. Success is HTTP 200. Invalid/failed helper operations return 422 with `{"error":"..."}`; transport failures return 502. Authentication failures return 401. An unavailable observation is an error, never a guessed running/stopped status.

- `GET /healthz`: live prerequisite health, including a `host` object in native mode. No control operation is performed.
- `GET /host/inventory`: `{ "slots": [{"id":"project-a","label":"...","workspace_id":null,"limits":{...}}] }`.
- `GET /host/workspaces`: `{ "workspaces": [<observation>] }`.
- `GET /host/workspaces/{id}`: current observation.
- `POST /host/workspaces/{id}/actions`: one of the actions below. IDs match `[a-z0-9][a-z0-9-]{0,47}`.

| Action body | Effect/result |
| --- | --- |
| `{"operation":"configure","slot":"project-a","limits":{"cpu_percent":200,"memory_mb":2048,"tasks":512}}` | Persistent registration, returning observation. Limits may be omitted and default to inventory ceilings. Account mapping is immutable; resource changes require stop. |
| `{"operation":"start"}` | Idempotent namespace/service start and terminal readiness. |
| `{"operation":"stop"}` | Stop the whole owned cgroup, clean up owned network resources, preserve files. |
| `{"operation":"remove"}` | Stop and unregister, preserve project and home files; returns `files_preserved:true`. |
| `{"operation":"list-files"}` | `{ "files": ["src/main.ex", ...] }`. |
| `{"operation":"read-file","path":"src/main.ex"}` | `{ "contents": "..." }`. |
| `{"operation":"write-file","path":"src/main.ex","content":"..."}` | `{ "status":"ok" }`; parent directories must already exist. |
| `{"operation":"delete-files","confirmation":"<exact workspace id>"}` | Separately confirmed project-content deletion, while stopped. Keeps project root and home. |
| `{"operation":"put-application","name":"web","port":3000}` | Register/update an HTTP target. Names match `[a-z][a-z0-9-]{0,31}`, ports 1024..65535, maximum 32 apps. Returns observation. |
| `{"operation":"remove-application","name":"web"}` | Close the application mapping; preserve files/processes. |

Observations include `id`, `slot`, `status` (`stopped`, `starting`, `running`, `stopping`, `failed`), `unit`, `netns`, `project`, `user`, `limits`, `applications` (name to port), `updated_at`, `error`, and `service_result`. Registration is authoritative in `/var/lib/orqos/host`. Systemd is authoritative for runtime state. The daemon serializes helper actions using a filesystem lock, so concurrent starts cannot duplicate services.

## Terminal and application forwarding

Authenticated HTTP and WebSocket ingress is available at:

- `/host/workspaces/{id}/terminal/` and its descendants. The browser-facing ttyd base path is `/terminal/{id}/`.
- `/host/workspaces/{id}/applications/{name}/proxy/` and its descendants. The proxy prefix is removed while preserving encoded path and query.

Ingress resolves only registered resources in a running owned workspace. It connects to a Unix socket, verifies the server's Unix UID and owned systemd cgroup before sending HTTP, and strips its own bearer credential. A workspace cannot redirect this privileged connection to Docker or another host socket. HTTP bodies stream and WebSocket upgrades carry bidirectional byte streams. App readiness failures return 502. An unprivileged forwarder inside the workspace namespace connects exclusively to its registered app's `127.0.0.1:<port>`. Public/private policy belongs to RawPair, whose reverse proxy must authenticate every ingress request. Never publish these endpoints directly.

## Lifecycle and ownership

Each registration has an unpredictable persisted token used in its service description, namespace name, interface alias, firewall table comment, and runtime directory. Systemd unit properties, namespace device/inode, and interface/firewall ownership are verified before adoption or cleanup. A durable creation-intent journal plus the full random name and root-owned handle recovers the narrow crash window before the namespace inode was saved. An ownership mismatch refuses the operation and requires operator inspection. No global firewall table is flushed. Dedicated tmux, ttyd, forwarders, shells, databases, and user-launched processes all live in one service cgroup under the configured Unix UID. CPUQuota, MemoryMax (no swap), TasksMax, privilege restrictions and filesystem protections apply to the service. Workspace homes must be under `/srv/rawpair`, outside `/home`, because the service uses `ProtectHome=yes`.

Browser disconnect and RawPair/Orqos restarts leave active workspace services running. A periodic reconciler removes network resources of stopped or failed services. Host reboot does not automatically start transient workspace services: explicit Start recovers registration and networking. A failed start attempts complete cleanup and records its error. `journalctl -u orqos-host` shows control failures; the unit name in the observation selects `journalctl -u <unit>` for terminal/workspace errors. Stop preserves project files. Remove releases the inventory slot, whose existing project files are still present when reused; the UI must explain this. Delete Files is an independent destructive action.

## Networking and files

Every workspace receives a /30 in the configured private /16, its own loopback, a virtual link, default route, DNS, and NAT to the existing host network. The installer explicitly enables IPv4 forwarding. Owned nftables rules drop access from a workspace to host inputs, private/reserved destinations, other workspaces, unsolicited incoming traffic, and direct host-origin connections into workspace interfaces. Registered Unix-socket ingress is the supported access path. IPv6 is disabled inside the namespace. The host address range must not conflict with existing routes/VPNs. Existing UFW/firewalld/nftables forwarding policy is preserved and may need an operator allow rule for workspace outbound/reply traffic; never disable an existing firewall wholesale. Public IPv4 DNS resolvers are required in this version; private LAN dependencies and IPv6 egress are deferred.

Project file operations fork and permanently drop supplementary groups, GID and UID before opening the configured project root. Directory-relative openat2 resolution rejects symlinks, traversal and submount crossings. Editor files are UTF-8 text up to 2 MiB, without NULs or multiple hard links. Listings exclude common dependency/build directories and cap at 20,000 files / depth 32. Saves are in-place with fsync, retain permissions, and are not crash-atomic. The terminal remains the interface for arbitrary binary files, directories, Git and builds. Toolchains are host-installed read-only or user-local in the workspace home; no images or automatic language/database installer is involved.

This MVP is for trusted collaborators. Separate Unix identities, cgroups, namespaces and service restrictions are useful boundaries, but native host execution shares the kernel and is not hostile multi-tenant isolation.
