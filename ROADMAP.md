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

Deliver the [native host backend for RawPair](docs/HOST_BACKEND.md), coordinated by [RawPair goal.txt](../rawpair/goal.txt). This is a new implementation milestone; current container validation does not establish host support.

- [ ] Explicit host-only startup without Docker or Kubernetes prerequisites.
- [ ] Owned native workspace lifecycle, Unix identities, systemd supervision, and resource limits.
- [ ] Per-workspace network namespaces, DNS/outbound access, and scoped cleanup.
- [ ] Safe native file operations and protected terminal/application forwarding.
- [ ] RawPair integration and usable native setup/service assets.

For this milestone, checks are best effort and are not completion gates. Completion requires the actual integrated MVP and operator handoff; the user will build, deploy locally, and use it afterward.

Continue maintaining the existing container checks and validate a Podman Unix socket before claiming equivalent runtime coverage.

## Later

- Kubernetes metrics through the Metrics Server API.
- Pod log streaming over WebSockets.
