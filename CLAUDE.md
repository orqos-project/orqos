# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

The imported `AGENTS.md` is the maintained repository guide (structure, commands, testing, style, configuration and the native host handoff). This file adds an architecture summary; user-facing behavior is in `README.md`, `docs/HOST_API.md` and `docs/PACKAGES.md`.

## Backends

Orqos is a single-binary Rust daemon (Axum HTTP/WebSocket API, OpenAPI via utoipa, Swagger UI at `/swagger`). `ORQOS_BACKENDS` selects `host`, `docker`, `kube` or `both` (default `both`, meaning Docker and Kubernetes). `src/backends.rs` parses the choice; `src/router.rs` mounts routes only for selected backends.

- **Native host** (`ORQOS_BACKENDS=host`, Linux only): `src/host.rs` serves the bearer-authenticated `/host` API (`ORQOS_HOST_TOKEN`, at least 32 bytes; loopback bind only) and terminal/application forwarding. Privileged workspace, networking and safe-file operations live in the Python helpers under `host/`. Install, provisioning and release assets are in `packaging/`. Host mode starts without probing Docker or Kubernetes and does not mount their routes.
- **Docker/Podman** (`src/routes/docker/`): containers via bollard. Rezn consumes this API; preserve its routes and semantics.
- **Kubernetes** (`src/routes/kube/`): pods via kube-rs.
- **Shared** (`src/routes/shared/`): `/healthz`, `/metrics`, `/events/ws`, `/stats/ws`, plus exec/file helpers reused by both container backends.

## State

- `AppState` (`app_state.rs`): passed to handlers as `Arc<AppState>`; holds the selected backends, `Option<Arc<DockerState>>`, `Option<Arc<KubeState>>`, shared broadcast channels (`events_tx`, `stats_tx`) and the `MetricRegistry`.
- `DockerState` / `KubeState`: backend clients. Container routes sit behind `require_docker` / `require_kube` middleware.
- `metric_poller.rs` and `metric_registry.rs`: Docker stats polling into a 60-second rolling window; `stats.rs` pushes aggregates to the stats stream.
- `spawn_docker_events_fanout.rs` / `spawn_kube_events_fanout.rs`: self-healing event listeners; events are wrapped as `{"source": "docker"|"kube", "event": ...}`.

## Patterns

- Container file I/O streams tar archives through the backend API; `write_file`/`read_file` reuse exec for chown/chmod and existence checks. Reads are confined to `ORQOS_READ_BASE` (default `/home`).
- WebSocket exec streams end with a `__exit_code:<N>` sentinel message.
- Metrics use the legacy `rezn_` prefix.

## Checks

- Rust: `cargo fmt --check`, `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`.
- Python host and packaging helpers: `python3 -m unittest discover -s host -p 'test_*.py' -v` and `python3 -m unittest discover -s packaging -p 'test_*.py' -v`.
- Live container smoke checks: `scripts/smoke.py` (see `docs/SMOKE.md`).
