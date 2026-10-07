# Orqos

**Orqos** is a small Rust daemon that manages Docker/Podman containers and Kubernetes pods through an HTTP/WebSocket API.

It provides lifecycle operations, command execution, file transfer, and event streams. Docker also supplies metrics and stats. Backend routes are mounted according to startup configuration.

The core MVP is validated locally on Docker Desktop and Kubernetes. See the [assessment](docs/ASSESSMENT.md) for evidence and limits, and the [roadmap](ROADMAP.md) for priorities.

## Local Development

Use Linux, a Rust toolchain, and an accessible Docker/Podman socket or Kubernetes configuration.

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

Startup requires one reachable enabled backend. `/healthz` probes enabled backends, returning 200 when all respond and 503 when degraded. Unavailable backend routes return 503 with details; disabled routes return 404. Restart after fixing configuration/client-construction errors; existing clients are probed again on requests.

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

* Used by [RawPair](https://github.com/rawpair/rawpair) to start per-user development containers
* Used by [Rezn](https://github.com/rezn-project/rezn) as the execution layer for declarative container orchestration
* Can be embedded in CI runners, custom PaaS setups, or your own tools
