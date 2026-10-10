# Repository Guidelines

## Native Host MVP Handoff

For RawPair native host work, read `../rawpair/goal.txt`, `../rawpair/docs/HANDOFF.md`, and `docs/HOST_BACKEND.md` first. RawPair's goal is the canonical brief. The native implementation and API are described in `docs/HOST_API.md`; `docs/HOST_BACKEND.md` preserves the original design brief. Preserve the existing Docker/Kubernetes contracts and coordinate the host API with RawPair.

The user explicitly requires a delivered, integrated MVP and usable operator setup. Builds, automated tests, CI, live checks, local deployment, and user acceptance are best-effort evidence, not completion preconditions for this milestone. Run useful checks when practical and fix discovered defects; report unverified behavior honestly. Missing core implementation still means unfinished work. This user policy governs the host milestone.

## Project Structure & Module Organization

Orqos is a single-binary Rust 2021 daemon exposing an Axum HTTP/WebSocket API for Docker/Podman containers and Kubernetes pods.

- `src/main.rs` initializes backends, background tasks, and HTTP serving; `src/router.rs` mounts routes for available backends.
- `src/routes/docker/` and `src/routes/kube/` contain backend handlers; `src/routes/shared/` contains metrics and event/stat streams.
- `app_state.rs`, `docker_state.rs`, and `kube_state.rs` hold shared and backend state. Metrics polling, storage, and event fanout live in dedicated `src/` modules.
- CI builds release artifacts and runs push/PR checks. Regression tests live beside their Rust modules; live smoke checks are in `scripts/smoke.py`. Swagger UI is generated at `/swagger/`.

## Build, Test, and Development Commands

Run commands from the repository root:

- `cargo build` — compile a debug binary.
- `cargo run` — start locally; requires a usable Docker/Podman connection or Kubernetes configuration.
- `cargo build --release --target x86_64-unknown-linux-gnu` — reproduce the release CI build; install the target with `rustup target add x86_64-unknown-linux-gnu` first.
- `cargo fmt --check` — check formatting; use `cargo fmt` to apply it.
- `cargo clippy` — run Rust lint checks.
- `cargo test --locked` — run the existing regression tests.

## Coding Style & Naming Conventions

Use rustfmt defaults with four-space indentation. Name files, modules, and functions in `snake_case`, types in `PascalCase`, and constants in `SCREAMING_SNAKE_CASE`. Follow existing handler names such as `create_container_handler` and `kube_read_file_handler`.

Keep backend logic in its corresponding route directory and shared infrastructure on `AppState`. Update route registration and `utoipa` annotations when changing APIs so generated documentation matches behavior.

## Testing Guidelines

Existing module-local regression tests and push/PR CI cover the container backends. Add focused Rust tests where useful, using `#[test]` or `#[tokio::test]` for asynchronous behavior and descriptive names such as `rejects_parent_directory_traversal`. Document setup and cleanup for checks requiring live backends. For the native host milestone, follow the best-effort validation policy above; test or live-check execution is not a completion gate.

## Commit & Pull Request Guidelines

History uses descriptive subjects such as “Add read_file route” and “Introduced BIND_ADDR ENV variable”; no Conventional Commits scheme is evident. Keep commits focused. PRs should explain behavior changes, link relevant issues, list validation commands and results, and include request/response examples for API changes.

## Configuration Tips

`BIND_ADDR` defaults to `127.0.0.1:3000`; `KUBE_NAMESPACE` defaults to `default`. `DOCKER_SOCKET` selects the fallback Unix socket. Preserve file-path validation and the `ORQOS_READ_BASE` restriction, which defaults to `/home` for both backends.
