# Roadmap

The MVP supports Docker/Podman containers and Kubernetes pods. [Local evidence](docs/ASSESSMENT.md) and [repeatable smoke checks](docs/SMOKE.md) cover the core Docker/Kubernetes workflows.

## Core MVP

- [x] Exact file reads, including empty/binary files and path restrictions.
- [x] Correct Docker upload destinations, overwrite checks, and metadata failures.
- [x] WebSocket command encoding and exit sentinels on both backends.
- [x] Kubernetes exit codes and concurrent stdout/stderr capture.
- [x] Explicit backend modes, reachability checks, and `/healthz`.
- [x] Docker command overrides and documented pre-pull behavior.
- [x] Pod deletion events with a distinguishable event type.
- [x] Regression tests, push/PR CI configuration, and OpenAPI coverage.
- [x] Local lifecycle, exec, file-transfer, startup/failure, and cleanup smoke checks.

## Next

Run the new checks in hosted CI when this branch is pushed. Keep the smoke suite as the acceptance gate for API changes. Validate a Podman Unix socket before claiming equivalent runtime coverage.

## Later

- Kubernetes metrics through the Metrics Server API.
- Pod log streaming over WebSockets.
