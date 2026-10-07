# Repository Guidelines

## Project Structure & Module Organization

Orqos is a single-binary Rust 2021 daemon exposing an Axum HTTP/WebSocket API for Docker/Podman containers and Kubernetes pods.

- `src/main.rs` initializes backends, background tasks, and HTTP serving; `src/router.rs` mounts routes for available backends.
- `src/routes/docker/` and `src/routes/kube/` contain backend handlers; `src/routes/shared/` contains metrics and event/stat streams.
- `app_state.rs`, `docker_state.rs`, and `kube_state.rs` hold shared and backend state. Metrics polling, storage, and event fanout live in dedicated `src/` modules.
- `.github/workflows/rust.yml` builds release artifacts. There are currently no test or asset directories. Swagger UI is generated at `/swagger`.

## Build, Test, and Development Commands

Run commands from the repository root:

- `cargo build` — compile a debug binary.
- `cargo run` — start locally; requires a usable Docker/Podman connection or Kubernetes configuration.
- `cargo build --release --target x86_64-unknown-linux-gnu` — reproduce the release CI build; install the target with `rustup target add x86_64-unknown-linux-gnu` first.
- `cargo fmt --check` — check formatting; use `cargo fmt` to apply it.
- `cargo clippy` — run Rust lint checks.
- `cargo test` — run tests once added.

## Coding Style & Naming Conventions

Use rustfmt defaults with four-space indentation. Name files, modules, and functions in `snake_case`, types in `PascalCase`, and constants in `SCREAMING_SNAKE_CASE`. Follow existing handler names such as `create_container_handler` and `kube_read_file_handler`.

Keep backend logic in its corresponding route directory and shared infrastructure on `AppState`. Update route registration and `utoipa` annotations when changing APIs so generated documentation matches behavior.

## Testing Guidelines

No tests or coverage threshold are configured; CI currently builds only on published releases. Add Rust unit tests in module-local `#[cfg(test)]` blocks, using `#[test]` or `#[tokio::test]` for asynchronous behavior. Use descriptive names such as `rejects_parent_directory_traversal`. Cover validation, error responses, and changed behavior. Document backend setup and cleanup for tests requiring containers or pods.

## Commit & Pull Request Guidelines

History uses descriptive subjects such as “Add read_file route” and “Introduced BIND_ADDR ENV variable”; no Conventional Commits scheme is evident. Keep commits focused. PRs should explain behavior changes, link relevant issues, list validation commands and results, and include request/response examples for API changes.

## Configuration Tips

`BIND_ADDR` defaults to `127.0.0.1:3000`; `KUBE_NAMESPACE` defaults to `default`. `DOCKER_SOCKET` selects the fallback Unix socket. Preserve file-path validation and the `ORQOS_READ_BASE` restriction, which defaults to `/home` for both backends.
