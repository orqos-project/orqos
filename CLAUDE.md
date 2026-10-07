# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build                # debug build
cargo build --release      # release build
cargo run                  # run locally (needs Docker/Podman socket or Kube config)
cargo clippy               # lint
cargo fmt                  # format
```

Release CI builds target `x86_64-unknown-linux-gnu` specifically:
```bash
cargo build --release --target x86_64-unknown-linux-gnu
```

There are no tests in the project currently.

## Environment Variables

- `BIND_ADDR` — HTTP listen address (default: `127.0.0.1:3000`)
- `DOCKER_SOCKET` — path to Docker/Podman socket (default: `/var/run/docker.sock`)
- `KUBE_NAMESPACE` — default Kubernetes namespace (default: `default`)
- `ORQOS_READ_BASE` — allowed base path for Docker `read-file` endpoint (default: `/home`)
- `RUST_LOG` — tracing log level

## Architecture

Orqos is a single-binary Rust daemon that manages Docker containers and Kubernetes pods. It exposes an HTTP/WebSocket API built with **Axum** and auto-generates OpenAPI docs via **utoipa** (Swagger UI at `/swagger`).

At startup, it attempts to connect to Docker (via [bollard](https://crates.io/crates/bollard)) and Kubernetes (via [kube](https://crates.io/crates/kube)). At least one must succeed. Routes are conditionally mounted based on which backends are available.

### State Architecture

There are three state structs, following a "shared infrastructure, separate backends" pattern:

- **`AppState`** (`app_state.rs`) — Top-level state passed to all Axum handlers as `Arc<AppState>`. Holds `Option<Arc<DockerState>>`, `Option<Arc<KubeState>>`, shared broadcast channels, and the `MetricRegistry`.
- **`DockerState`** (`docker_state.rs`) — Docker-specific: `bollard::Docker` client + CPU snapshot cache.
- **`KubeState`** (`kube_state.rs`) — Kube-specific: `kube::Client` + default namespace.

Broadcast channels (`events_tx`, `stats_tx`) and `MetricRegistry` live on `AppState` so both backends can feed into them.

### Route Structure

Routes are organized by backend under `src/routes/`:

- **`routes/docker/`** — Docker container operations. Handlers access `app.docker.as_ref().unwrap()` (safe because routes are only mounted when Docker is available).
- **`routes/kube/`** — Kubernetes pod operations. Same pattern with `app.kube.as_ref().unwrap()`.
- **`routes/shared/`** — Backend-agnostic endpoints (events WS, stats WS, metrics). Always mounted.

### Key Modules

- **`router.rs`** — Conditional route mounting, Swagger UI. Docker routes under `/docker/`, Kube routes under `/kube/`, shared routes at root.
- **`metric_poller.rs`** — Polls Docker stats API per container; computes CPU % via delta snapshots. Takes `&DockerState` + `&MetricRegistry`.
- **`metric_registry.rs`** — Thread-safe rolling-window store using `DashMap<String, VecDeque<(Instant, f64)>>` with 60s max window. Generic (string-keyed), works for both backends.
- **`spawn_docker_events_fanout.rs`** — Background Docker event listener → shared `events_tx`. Idle-aware, self-healing with exponential backoff. Events wrapped as `{"source": "docker", "event": ...}`.
- **`spawn_kube_events_fanout.rs`** — Same pattern for Kube pod events via `kube::runtime::watcher`. Events wrapped as `{"source": "kube", "event": ...}`.
- **`stats.rs`** — Aggregates metrics from registry and pushes to stats WS channel.

### API Endpoints

#### Docker (`/docker/...`) — mounted only if Docker is available

| Method | Path | Description |
|--------|------|-------------|
| GET | `/docker/containers` | List containers (filters: status, label, name, all) |
| POST | `/docker/containers` | Create and start a container |
| POST | `/docker/containers/{id}/stop` | Stop a container |
| POST | `/docker/containers/{id}/remove` | Remove a container |
| POST | `/docker/containers/{id}/exec` | Execute command (buffered JSON response) |
| GET | `/docker/containers/{id}/exec/ws` | Execute command (live WebSocket stream) |
| POST | `/docker/containers/{id}/write-file` | Write file into container (via tar upload) |
| POST | `/docker/containers/{id}/read-file` | Read file from container |

#### Kubernetes (`/kube/...`) — mounted only if Kube is available

| Method | Path | Description |
|--------|------|-------------|
| GET | `/kube/pods` | List pods (filters: namespace, label_selector, field_selector) |
| POST | `/kube/pods` | Create a pod from JSON manifest |
| POST | `/kube/pods/{name}/delete` | Delete a pod |
| POST | `/kube/pods/{name}/exec` | Execute command in pod (buffered JSON response) |

#### Shared (always mounted)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/metrics` | Prometheus-style plain-text metrics |
| GET | `/events/ws` | Event WebSocket stream (Docker + Kube events) |
| GET | `/stats/ws` | Container stats WebSocket stream |

### Patterns

- Docker file I/O uses tar archives (`tar` crate) streamed through the Docker API, not CLI exec
- The `write_file` and `read_file` handlers reuse `exec_once_handler` internally for chown/chmod and existence checks
- WebSocket exec streams use a `__exit_code:<N>` sentinel message to signal process termination
- Metrics use prefix `rezn_` (legacy naming from the Rezn project)
- WebSocket events include a `"source"` field (`"docker"` or `"kube"`) so clients can distinguish origin
