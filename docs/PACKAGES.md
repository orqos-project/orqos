# Ubuntu native packages

`orqos-host` packages the native binary, Python execution helpers, systemd unit and local administrator command for Ubuntu 24.04 on amd64/arm64. Installation needs no source checkout or Rust toolchain. The Docker/Kubernetes APIs remain in the binary, and the standalone Linux binaries remain release assets.

Download `orqos-host_VERSION_ARCH.deb` and `SHA256SUMS` from the same immutable release. Verify its checksum, then install:

```sh
sha256sum --check --ignore-missing SHA256SUMS
sudo apt install ./orqos-host_VERSION_ARCH.deb
sudo orqos-host-admin check-subnet 10.231.0.0/16
sudo orqos-host-admin init --subnet 10.231.0.0/16
sudo orqos-host-admin provision project-a
sudo orqos-host-admin provision project-b
sudo orqos-host-admin validate
sudo systemctl enable --now orqos-host
```

For integrated RawPair installation, use RawPair's `rawpair-native` metapackage and `sudo rawpair setup`, which initialize Orqos and create the default slots. Do not initialize or provision them separately first unless intentionally preparing a package-managed Orqos installation for RawPair.

Fresh installation leaves Orqos unconfigured and stopped. Initialization generates its token once, installs an empty inventory, refuses overlapping private /16 routes, and enables IPv4 forwarding through its own sysctl file. Provisioning chooses the next unused network index, a dedicated locked/expired nologin identity and resource ceilings (200% CPU, 2048 MiB, 512 tasks). Repeating a provision command validates an owned slot instead of creating a second identity. A protected journal records interrupted account creation. Existing unrelated accounts, groups, homes and inventory mappings are refused.

The daemon binds only `127.0.0.1:9080`. Its API continues to require `/etc/orqos/host.env`'s token. `orqos-host-admin diagnose` reads inventory, restrictions, routes and prerequisites without creating files. `validate` also serializes with runtime operations. These commands add no HTTP endpoints.

Upgrade stops the control daemon while replacing execution assets; independently supervised workspace services and their processes remain running. Review and start `orqos-host` afterward, or run RawPair setup for a paired upgrade. Explicit removal stops the daemon, then verifies and stops every persisted owned workspace before removing helpers. Failure to stop safely aborts removal. `orqos-host-admin stop-all` does the same owned-resource stop without deleting records. Removal and purge preserve `/etc/orqos`, `/var/lib/orqos`, dedicated identities and `/srv/rawpair` homes. They never flush a shared firewall or remove unrelated units.

Older manual installations and packages without this package's ownership marker are refused before file overwrite. Back up inventory, token, runtime ownership, database and workspace homes; stop the old deployment, archive its units and execution paths, then install fresh on another host or restore through an explicit operator-managed migration. Do not invent package ownership markers or remove persisted runtime records to bypass detection. Automatic migration is deferred. Source installs use `packaging/install-host.sh [SUBNET]` and the same administrator helpers, with a distinct source ownership marker; conversion to package ownership requires migration.

# Releases

Set `Cargo.toml` and its lockfile package version together; tag `vMAJOR.MINOR.PATCH`. `.github/workflows/native-release.yml` builds natively on `ubuntu-24.04` and `ubuntu-24.04-arm` using Rust 1.99.0. Version mismatches and missing architecture assets fail the build. CI resolves dependencies, installs, initializes, retries provisioning, validates, reinstalls and removes the packages on disposable runners. Both architecture jobs must succeed before the workflow assembles a manifest/checksum list, creates GitHub build attestations and uploads a draft release. The operator reviews and publishes it. The workflow uses only repository-scoped `github.token` permissions, without a PAT or signing-key service.

For local packaging after `cargo build --release --locked`:

```sh
python3 packaging/build.py --tag v0.2.0 --arch amd64
python3 -m unittest discover -s packaging -p 'test_*.py' -v
```

Checksums verify download integrity, not signatures. Optional provenance verification is separate:

```sh
gh attestation verify orqos-host_VERSION_ARCH.deb --repo orqos-project/orqos
```

RawPair's compatibility manifest initially requires the new `v0.2.0` native-package release. Publish that reviewed Orqos release before building the paired RawPair release. No release has been published by implementing these files. Live systemd/network checks require a disposable privileged Linux host; source/unit tests alone do not establish a running installation.
