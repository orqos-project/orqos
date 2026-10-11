# Roadmap

The MVP supports native Linux workspaces, Docker/Podman containers and Kubernetes pods. [Local evidence](docs/ASSESSMENT.md) and [repeatable smoke checks](docs/SMOKE.md) cover the core Docker/Kubernetes workflows.

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

## Native host milestone: implemented

The [native host API](docs/HOST_API.md) is integrated with RawPair. The [package guide](docs/PACKAGES.md) covers standalone initialization, provisioning and source installation; RawPair's paired installer configures the complete browser experience.

- [x] Explicit host-only startup without Docker or Kubernetes prerequisites.
- [x] Owned native workspace lifecycle, Unix identities, systemd supervision and resource limits.
- [x] Per-workspace network namespaces, DNS/outbound access and scoped cleanup.
- [x] Safe native file operations and protected terminal/application forwarding.
- [x] RawPair integration, native setup/service assets and Ubuntu package/release pipelines.

## Next validation and release work

Build and review the first native release, then publish the pinned Orqos partner before RawPair's paired release. Run the configured Ubuntu 24.04 amd64/arm64 package lifecycle and privileged network checks, and complete interactive browser/wizard acceptance. [RawPair's package handoff](https://github.com/rawpair/rawpair/blob/main/docs/PACKAGE_HANDOFF.md) distinguishes implemented paths from checks actually run.

Continue maintaining the existing container checks and validate a Podman Unix socket before claiming equivalent runtime coverage.

## Later

- Kubernetes metrics through the Metrics Server API.
- Pod log streaming over WebSockets.
