# Orqos

**Run native development workspaces, containers and pods through a small HTTP/WebSocket service.**

Orqos is the execution layer behind [RawPair](https://github.com/rawpair/rawpair)'s native Linux workspaces. It starts and stops projects, keeps their files, supervises processes and provides protected terminal/application forwarding. RawPair adds the browser editor, shared terminal, logins, workspace membership and HTTPS previews.

Native mode uses systemd, dedicated Unix accounts, cgroup limits and per-workspace networks. Each project gets its own loopback: two projects can both run a server on port 3000. Browser disconnects and control-daemon restarts leave active workspaces running; stopping one workspace stops its processes and preserves its files.

Docker/Podman and Kubernetes APIs remain available for container integrations. You select the backend for the service you run.

## Choose your first try

| You want to… | Start here |
| --- | --- |
| Work in a shared browser editor/terminal with a coworker | [Install RawPair](https://github.com/rawpair/rawpair/blob/main/docs/PACKAGES.md); its installer includes and configures the tested Orqos pair |
| Explore native execution or build your own integration | [Install standalone Orqos](docs/PACKAGES.md), then use the [host API](docs/HOST_API.md) |
| Work with Docker/Podman or Kubernetes | [Local development](#local-development) and the [container API](#http-api) below |

For the complete collaborative experience, follow RawPair's [first-workspace walkthrough](https://github.com/rawpair/rawpair/blob/main/docs/FIRST_WORKSPACE.md). Serve a small page, edit it together, then try two projects on the same port. Share that guide with a teammate who would like to pair with you.

Native `.deb` packages target **Ubuntu 24.04 and 26.04, amd64 and arm64**, with no Rust toolchain needed to install. Choose a [published native release](https://github.com/orqos-project/orqos/releases) containing `orqos-host` packages and `SHA256SUMS`. The pipeline is implemented; if the assets are absent, use the [source path](docs/PACKAGES.md#build-native-mode-from-source). RawPair users should use its paired installer instead of installing Orqos independently.

## Native mode

`ORQOS_BACKENDS=host` starts without Docker or Kubernetes. The installed `orqos-host` service binds to `127.0.0.1:9080` and requires a bearer token for every `/host` request. It provides:

- Persistent workspace registration and observable start/stop/remove operations.
- Dedicated accounts, process resource limits and scoped network setup/cleanup.
- Project file listing, reads and saves under the workspace's Unix permissions.
- Internal HTTP/WebSocket forwarding to a shared terminal or a registered application.
- Ownership checks and restart recovery that refuse unrelated host resources.

Orqos is a local execution service; authentication for people, collaboration, HTTPS and public/private browser policy belong to RawPair or your integration. Native mode targets one systemd Linux host and trusted collaborators. It shares the host kernel and is not hostile multi-tenant isolation.

See [package setup and operations](docs/PACKAGES.md), the [concrete API](docs/HOST_API.md), and the [original design brief](docs/HOST_BACKEND.md). Native implementation is delivered; Ubuntu package lifecycle and full privileged/browser acceptance still need live evidence as recorded in the [RawPair package handoff](https://github.com/rawpair/rawpair/blob/main/docs/PACKAGE_HANDOFF.md). Existing [local container evidence](docs/ASSESSMENT.md) covers Docker/Kubernetes only.

## Local Development

The commands in this section run container backends. Use Linux, a Rust toolchain, and an accessible Docker/Podman socket or Kubernetes configuration. For native mode, follow the [installed helper/service setup](docs/PACKAGES.md#build-native-mode-from-source) instead of an unconfigured `cargo run`.

```bash
cargo build --locked
cargo run --locked
```

The first build downloads Swagger UI and needs network access. The default HTTP address is `127.0.0.1:3000`; override it with `BIND_ADDR`.

Select backends with `ORQOS_BACKENDS=docker|kube|both` (default: `both`). For Docker Desktop:

```bash
ORQOS_BACKENDS=both \
DOCKER_HOST="$(docker context inspect desktop-linux --format '{{.Endpoints.docker.Host}}')" \
cargo run --locked
```

Both local engines can remain running. Socket precedence is `DOCKER_SOCKET`, then Unix `DOCKER_HOST`, then `/var/run/docker.sock`. TCP Docker hosts are unsupported. Kubernetes uses kubeconfig or in-cluster configuration; `KUBE_NAMESPACE` defaults to `default`. Pin a kubeconfig containing the intended context when running locally.

Container startup requires one reachable enabled backend. `/healthz` probes enabled backends, returning 200 when all respond and 503 when degraded. Unavailable backend routes return 503 with details; disabled routes return 404. Restart after fixing configuration/client-construction errors; existing clients are probed again on requests.

## HTTP API

```bash
curl 'http://127.0.0.1:3000/docker/containers?all=true'
curl 'http://127.0.0.1:3000/kube/pods'
```

Swagger UI is at `/swagger/`, with OpenAPI JSON at `/api/openapi.json`. Shared endpoints include `/events/ws`, `/stats/ws`, and `/metrics`; metrics and stats cover Docker.

Docker creation requires pre-pulled images (missing resources return 404); optional `cmd: ["sleep", "600"]` overrides the image command. Pull into the selected engine with `docker --context desktop-linux pull alpine:3.22`.

WebSocket exec uses one URL-encoded JSON array in `cmd`, for example `new URLSearchParams({cmd: JSON.stringify(["echo", "hello"])})`. Both backends send `{"stream":"stdout|stderr","data":"..."}` frames, followed by `__exit_code:N`; `-1` means unavailable exit status. REST exec retains an array in the JSON body.

File writes accept UTF-8 `content`; reads return raw bytes under `ORQOS_READ_BASE` (default `/home`). Directories, symlinks and parent traversal are rejected. Kubernetes file transfer requires `tar`; metadata changes require `chmod`/`chown`. Permission errors may leave a written file. Kubernetes events retain `source: "kube"` and add `type: "APPLIED"|"DELETED"`; streams are best effort.

## Validation

Run `cargo fmt --check`, `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings`. [Smoke checks](docs/SMOKE.md) verify real backends and clean up their fixtures. CI uses Rust 1.99.0 for checks and release builds. Push/PR CI runs these Rust checks, a build, and Docker smoke checks; published releases retain artifact builds.

## Use Cases

* Native execution for [RawPair](https://github.com/rawpair/rawpair): multiple projects, shared terminal access, private previews and independent workspace stop.
* Used by [Rezn](https://github.com/rezn-project/rezn) as the execution layer for declarative container orchestration
* Can be embedded in CI runners, custom PaaS setups, or your own tools

## Try it and share what you learn

Pick the [RawPair walkthrough](https://github.com/rawpair/rawpair/blob/main/docs/FIRST_WORKSPACE.md) for pairing, or the [standalone package guide](docs/PACKAGES.md) for an integration. If you build something with Orqos, [tell us about it or report what got in your way](https://github.com/orqos-project/orqos/issues). Include your version and backend, and remove bearer tokens and private configuration from logs.
