> Historical design brief, recorded 2026-10-10. Native mode is implemented; the future-tense text below preserves the original requirements. For current installation use [PACKAGES.md](PACKAGES.md); for the implemented contract use [HOST_API.md](HOST_API.md).

# Native host backend for RawPair

Orqos will provide the execution layer for RawPair's native Linux development workspaces. The feature is planned as of 2026-10-10; current Orqos supports Docker/Podman and Kubernetes only. The canonical scope and delivery policy are [RawPair goal.txt](https://github.com/rawpair/rawpair/blob/main/goal.txt), [the product plan](https://github.com/rawpair/rawpair/blob/main/docs/HOST_MVP.md), and [the session handoff](https://github.com/rawpair/rawpair/blob/main/docs/HANDOFF.md).

The user wants a delivered implementation and usable setup, with checks performed as far as practical and hands-on deployment/use afterward. Builds, tests, CI, and live checks are best-effort evidence, not completion gates. Existing assessment and smoke results establish container-backend behavior only.

## Execution responsibilities

Target one Linux systemd host with trusted collaborators. Orqos owns native runtime operations: workspace registration, service lifecycle, Unix identity, resource limits, safe file operations, networking, internal terminal/application endpoints, ownership recovery, and truthful status. RawPair owns user login, membership, collaboration, and public/private application policy.

Keep ttyd and tmux as the terminal components. Orqos runs and supervises them under workspace identities. It does not need a new interactive terminal protocol or a Rezn desired-state controller.

## Backend selection and API contract

Add explicit host-only startup, for example `ORQOS_BACKENDS=host`, without a Docker socket, kubeconfig, or container-backend probe. This setting is planned, not available in the current binary. Preserve the meaning and routes of existing `docker`, `kube`, and `both` modes; `both` must continue to mean Docker plus Kubernetes. Mixed host/container modes can follow if needed, but are not required for this milestone.

Extend backend state, route registration, health, errors, and OpenAPI together. Host availability should describe actual service/helper access and required networking support. Missing privileges or prerequisites produce actionable failures, not a nominally running workspace. Existing Docker stats are not host workspace metrics; do not relabel them as such.

Define the concrete host route names and request/response schemas during implementation. The minimum contract needs:

| Operation | Required behavior |
| --- | --- |
| Register/configure | Stable workspace ID, permitted account/project root, limits, and persistent owned configuration. |
| List/inspect | Configured and observed runtime state, timestamps, owned unit/namespace identity, and useful errors. |
| Start | Idempotent creation of the workspace service, tmux/ttyd session, and namespace networking. |
| Stop | Idempotent termination of the workspace descendants and cleanup of owned runtime/network resources; preserve files. |
| Remove runtime/configuration | Coordinate stop and removal without silently deleting project data or orphaning owned processes. |
| Files | List, read, and save files under the configured project root with workspace Unix permissions. |
| Terminal connection | Return a protected internal ttyd endpoint; browser authorization remains in RawPair's gateway. |
| Application registration | Named HTTP application and valid internal port belonging to a registered workspace. |
| Application connection | Protected forwarding into that workspace, including a localhost-only development server. |

Requests for a shared workspace act on its configured runtime identity, never an arbitrary caller-supplied host user. Restrict internal proxy targets to registered workspace endpoints. Keep public/private browser policy in RawPair; Orqos should never make its control API or internal forwarding generally public.

RawPair connects over an authenticated local transport. A permission-controlled Unix socket or authenticated loopback API is acceptable; loopback alone is not authorization. Use a narrow privileged helper or equivalently constrained service design for operations requiring host privileges. Do not turn the public API into arbitrary root command execution or accept unrestricted systemd properties, executable paths, environment, unit names, file roots, or namespace paths. Workspace launch configuration is operator controlled; users still execute ordinary development commands inside their unprivileged workspace terminal.

## Identity and process lifecycle

Map each workspace to a dedicated Unix account, persistent home/project directories, clean environment, runtime directory, tmux socket, owned systemd service, and namespace. Preprovisioned accounts are sufficient. Record these mappings under control-plane ownership so a workspace cannot rewrite its permissions or launch specification.

Start the dedicated tmux server inside the workspace service. Its descendants must remain in the managed cgroup together with ttyd and user processes. Configure CPU, memory, and task limits, appropriate filesystem restrictions, and privilege constraints. Phoenix stays unprivileged; workspace shells receive neither unrestricted sudo nor host-management credentials.

Separate desired registration from observed state. Persist enough ownership information to rediscover runtimes after an Orqos restart without duplicate launches or mutation of unrelated services. A unit-name prefix or reused PID is insufficient ownership evidence. Scope all filesystem, process, namespace, interface, and firewall cleanup to verified owned resources.

Browser disconnection leaves the service running. Stop terminates the workspace process tree and removes runtime networking while preserving files and registration. Failed starts unwind partial resources and report what failed. Restarting RawPair or Orqos must not kill or duplicate otherwise healthy workspaces. Host reboot may leave workspaces stopped; document the implemented startup policy and support predictable explicit start.

Existing user-launched applications need not become individually supervised production services. Their processes belong to the workspace, survive terminal-client disconnection through tmux, and stop with it. Logs and status must be sufficient for the operator to diagnose a failed workspace or application connection.

## Network namespaces and host connectivity

Use one network namespace per workspace. Independent loopback and port spaces are required so two projects can both listen on port 3000 and run a database on port 5432. Namespace setup is part of the host MVP, not a later replacement for a shared global port allocator.

Provide namespace handles, virtual links, private addresses, routing, DNS, outbound connectivity, and scoped firewall/NAT policy. A normal physical interface can carry all workspace outbound traffic through the host. No special external router, VLAN support, extra network card, or public address per workspace is needed.

Resolve DNS inside the workspace correctly; the host's loopback resolver is not automatically usable there. Define cross-workspace and host-management access deliberately. Limit privilege-bearing network operations to owned resources and narrowly scoped rule sets. Avoid flushing global firewall tables, changing unrelated routes, or taking ownership of existing namespaces.

Systemd can join a configured namespace through `NetworkNamespacePath`; iproute2 can manage namespace handles and virtual links. Choose the smallest suitable implementation and document required kernel, systemd, privilege, address-range, routing, and firewall prerequisites. The user should not manually construct networking for each project.

A process bound to a workspace's `127.0.0.1` is not reachable at the host's `127.0.0.1`. Provide an in-namespace forwarder or equivalent protected connector for registered application ports, so localhost-only development servers can be previewed. ttyd may use a protected filesystem Unix socket or another gateway-controlled internal endpoint. Host-accessible routes and forwarding sockets must not bypass workspace policy.

Stop and failed-start cleanup must account for running processes and open namespace references, as well as virtual links and owned firewall rules. A stale application mapping must never start forwarding to a different workspace after an address is reused.

## File operations

Native list/read/save operations use workspace Unix permissions and a configured project root. Resolve paths safely using directory-relative operations and enforced symlink/traversal rules; checking a string prefix or realpath before a later privileged open does not close replacement races. Rejecting symlink traversal is acceptable for the MVP.

Use useful limits and errors for malformed paths, permissions, unsupported file types, and large files. Saving a file must not imply permission to chown arbitrary host paths or execute privileged commands. Document overwrite and atomic-save behavior. Existing Docker/Kubernetes `ORQOS_READ_BASE` rules remain unchanged; the host contract needs an explicit per-workspace root rather than an unrestricted host path.

## Application preview boundary

Orqos registers application identities and forwards to their internal endpoints. RawPair and its reverse proxy provide HTTPS, user authorization, private/public visibility, and WebSocket upgrades. Multiple named applications can belong to one workspace. Application removal closes its forwarding without deleting workspace data.

Do not accept arbitrary upstream URLs from browser requests. Limit ports and protocols to the registered host-workspace contract. Protect the control API and internal sockets; knowing a workspace ID is not permission to use its terminal or apps. Keep management session credentials out of application traffic. Generic TCP/UDP publication and production ingress orchestration are deferred.

## Implementation entry points

- `src/backends.rs`: explicit selection and host prerequisite discovery.
- `src/main.rs`, `src/app_state.rs`, and `src/router.rs`: backend state, startup, route mounting, and OpenAPI.
- `src/routes/shared/health.rs`: host health without mandatory Docker/Kubernetes availability.
- New host backend modules: persisted configuration/ownership, constrained helper/service interaction, lifecycle, filesystem operations, namespace networking, and forwarding.
- Existing `src/routes/docker/` and `src/routes/kube/`: preserve their public contracts and behavior.

The current API has no host routes or host-mode configuration. Document actual endpoints, configuration, service/helper installation, and operator commands alongside implementation. Keep the first version focused; avoid a broad rewrite of existing backends or dependency upgrades unrelated to delivery.

## Delivery and validation policy

Deliver the real backend integrated with RawPair, plus native configuration and service/setup assets. Update the RawPair handoff with concrete build/install/start commands, required permissions, logs, recovery, and cleanup. The user will build and deploy locally and then report problems from actual use.

Run useful formatting, build, regression, or live checks when practical, and fix defects they reveal. Report results honestly. Automated tests, green CI, privileged live checks, and successful local deployment are not goal-completion preconditions for this milestone. Do not stop at scaffolding or omit a required capability because checks are optional. Existing evidence in `docs/ASSESSMENT.md` remains evidence for Docker/Kubernetes only.

## References

- [Linux network namespaces](https://www.man7.org/linux/man-pages/man7/network_namespaces.7.html) and [ip netns](https://www.man7.org/linux/man-pages/man8/ip-netns.8.html).
- [systemd execution settings](https://github.com/systemd/systemd/blob/main/man/systemd.exec.xml), including `NetworkNamespacePath` and privilege/filesystem controls.
- [systemd resource controls](https://github.com/systemd/systemd/blob/main/man/systemd.resource-control.xml) and [process termination](https://github.com/systemd/systemd/blob/main/man/systemd.kill.xml).
- [ttyd options](https://github.com/tsl0922/ttyd#usage).
