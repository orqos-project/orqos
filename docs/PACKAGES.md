# Install standalone native Orqos

Use this guide to run the native execution service or build an integration with its local API. For shared editing, a browser terminal and HTTPS previews with friends/coworkers, [install RawPair instead](https://github.com/rawpair/rawpair/blob/main/docs/PACKAGES.md). Its installer includes the exact tested Orqos package and configures the default workspace slots; you do not need this standalone procedure first.

`orqos-host` contains the native binary, Python execution helpers, systemd unit and local administrator command. Prebuilt installation needs no source checkout or Rust toolchain. Docker/Kubernetes APIs and standalone binary downloads are retained; the native package's service runs only the host backend.

## Before you start

Use **Ubuntu 24.04 amd64 or arm64**, booted with systemd/cgroup v2, with sudo access and internet access to GitHub and Ubuntu repositories. Reserve a non-overlapping private IPv4 `/16` for workspaces; the default is `10.231.0.0/16`. The daemon needs free loopback port **9080**, and workspace networking needs IPv4 forwarding plus outbound/reply permission in any existing host firewall.

Open [Orqos Releases](https://github.com/orqos-project/orqos/releases) and choose a published native release containing `orqos-host_VERSION_ARCH.deb` and `SHA256SUMS`. The pipeline is implemented, but its assets require a reviewed, published release. If those assets are absent, follow [source installation](#build-native-mode-from-source).

## Download and install

Run as your ordinary sudo-capable user. This example uses the native tag `v0.2.2`; choose a published tag and matching package version from its asset list if different. `dpkg --print-architecture` must report `amd64` or `arm64`.

```sh
sudo apt update
sudo apt install -y ca-certificates curl python3
mkdir -p ~/orqos-install
cd ~/orqos-install
ORQOS_TAG=v0.2.2
ORQOS_VERSION=0.2.2
ORQOS_ARCH=$(dpkg --print-architecture)
ORQOS_PACKAGE="orqos-host_${ORQOS_VERSION}_${ORQOS_ARCH}.deb"
ORQOS_BASE="https://github.com/orqos-project/orqos/releases/download/$ORQOS_TAG"
curl --fail --show-error --location --proto '=https' --output SHA256SUMS "$ORQOS_BASE/SHA256SUMS" &&
  curl --fail --show-error --location --proto '=https' --output "$ORQOS_PACKAGE" "$ORQOS_BASE/$ORQOS_PACKAGE" &&
  sha256sum --check --ignore-missing SHA256SUMS &&
  sudo apt install "./$ORQOS_PACKAGE"
```

The downloaded package must report `OK`. Stop if a download or checksum check fails. Checksums detect changed downloads; they are not signatures. Optional GitHub provenance verification is described [below](#releases). apt resolves runtime dependencies, including Python, ttyd/tmux, network tools and required shared libraries. Installation leaves the daemon unconfigured and stopped.

## Initialize and check the service

Check the proposed subnet before initialization. If it overlaps a host route, choose another private `/16` in both commands. Keep the chosen subnet once registrations exist.

```sh
sudo orqos-host-admin check-subnet 10.231.0.0/16
sudo orqos-host-admin init --subnet 10.231.0.0/16
sudo orqos-host-admin provision project-a
sudo orqos-host-admin provision project-b
sudo orqos-host-admin validate
sudo systemctl enable --now orqos-host
curl --fail --show-error http://127.0.0.1:9080/healthz
```

A healthy response reports `status: "ok"` and an available host backend. The daemon is now ready, with two account slots. Provisioning a slot does not register or start a workspace; your integration uses the [host API](HOST_API.md) for that. For the browser experience, use [RawPair's first-workspace guide](https://github.com/rawpair/rawpair/blob/main/docs/FIRST_WORKSPACE.md) on a RawPair installation.

## Read your inventory through the API

Every `/host` request needs the token in `/etc/orqos/host.env`. This read-only example runs on the host, reads the protected token without printing it, and lists available slots using Python already installed with the package:

```sh
sudo python3 - <<'PYTHON'
import json
from pathlib import Path
import urllib.request

entries = dict(line.split('=', 1) for line in Path('/etc/orqos/host.env').read_text().splitlines() if line)
request = urllib.request.Request(
    'http://127.0.0.1:9080/host/inventory',
    headers={'Authorization': 'Bearer ' + entries['ORQOS_HOST_TOKEN']},
)
with urllib.request.urlopen(request, timeout=10) as response:
    print(json.dumps(json.load(response), indent=2))
PYTHON
```

Expect slots named `project-a` and `project-b` with no assigned `workspace_id` on a fresh installation. Next, use the [API contract](HOST_API.md#contract) to configure a workspace in a slot and start it. Your integration owns user authorization and HTTPS policy. Keep the daemon on loopback and never send this control token to browsers or workspace accounts.

## Operating and troubleshooting

```sh
sudo orqos-host-admin diagnose
sudo systemctl status orqos-host --no-pager
sudo journalctl -u orqos-host -n 100 --no-pager
```

A subnet conflict needs another non-overlapping range before initialization. An ownership conflict needs inspection or the explicit migration below; keep persisted records. If outbound workspace networking fails, inspect public IPv4 DNS and forwarding/reply rules in your existing firewall. Never flush shared firewall tables. Workspace observations contain a `unit` name and any startup error; use `sudo journalctl -u UNIT_NAME` for that unit.

If you need help, [open an issue](https://github.com/orqos-project/orqos/issues) with the Orqos version, Ubuntu/CPU, backend and relevant diagnostic output. Remove tokens and private configuration first.

## Persistent state, upgrades and removal

Fresh installation leaves Orqos unconfigured and stopped. Initialization generates its token once, installs an empty inventory, refuses overlapping private /16 routes, and enables IPv4 forwarding through its own sysctl file. Provisioning chooses the next unused network index, a dedicated locked/expired nologin identity and resource ceilings (200% CPU, 2048 MiB, 512 tasks). Repeating a provision command validates an owned slot instead of creating a second identity. A protected journal records interrupted account creation. Existing unrelated accounts, groups, homes and inventory mappings are refused.

The daemon binds only `127.0.0.1:9080`. Its API continues to require `/etc/orqos/host.env`'s token. `orqos-host-admin diagnose` reads inventory, restrictions, routes and prerequisites without creating files. `validate` also serializes with runtime operations. These commands add no HTTP endpoints.

Upgrade stops the control daemon while replacing execution assets; independently supervised workspace services and their processes remain running. Review and start `orqos-host` afterward, or run RawPair setup for a paired upgrade. Explicit removal stops the daemon, then verifies and stops every persisted owned workspace before removing helpers. Failure to stop safely aborts removal. `orqos-host-admin stop-all` does the same owned-resource stop without deleting records. Removal and purge preserve `/etc/orqos`, `/var/lib/orqos`, dedicated identities and `/srv/rawpair` homes. They never flush a shared firewall or remove unrelated units.

Older manual installations and packages without this package's ownership marker are refused before file overwrite. Back up inventory, token, runtime ownership, database and workspace homes; stop the old deployment, archive its units and execution paths, then install fresh on another host or restore through an explicit operator-managed migration. Do not invent package ownership markers or remove persisted runtime records to bypass detection. Automatic migration is deferred. Source installs use `packaging/install-host.sh [SUBNET]` and the same administrator helpers, with a distinct source ownership marker; conversion to package ownership requires migration.

## Build native mode from source

This is the standalone source path on Ubuntu 24.04. RawPair source users should follow its [integrated guide](https://github.com/rawpair/rawpair/blob/main/docs/NATIVE_SETUP.md) instead. Install Rust/Cargo with your preferred toolchain manager, then:

```sh
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev git curl python3 iproute2 nftables tmux ttyd
mkdir -p ~/dev
cd ~/dev
git clone https://github.com/orqos-project/orqos.git
cd orqos
cargo build --release --locked
sudo packaging/install-host.sh 10.231.0.0/16
sudo orqos-host-admin provision project-a
sudo orqos-host-admin provision project-b
sudo orqos-host-admin validate
sudo systemctl enable --now orqos-host
curl --fail --show-error http://127.0.0.1:9080/healthz
```

The source installer initializes the chosen subnet; select a different non-overlapping private `/16` before running it if needed. It installs the same administrator helpers under a distinct source ownership marker. It does not silently convert to package management. Continue with the same [inventory/API example](#read-your-inventory-through-the-api).

## Releases

Maintainers: set `Cargo.toml` and its lockfile package version together; tag `vMAJOR.MINOR.PATCH`. `.github/workflows/native-release.yml` builds natively on `ubuntu-24.04` and `ubuntu-24.04-arm` using Rust 1.99.0. Version mismatches and missing architecture assets fail the build. CI resolves dependencies, installs, initializes, retries provisioning, validates, reinstalls and removes the packages on disposable runners. Both architecture jobs must succeed before the workflow assembles a manifest/checksum list, creates GitHub build attestations and uploads a draft release. The operator reviews and publishes it. The workflow uses only repository-scoped `github.token` permissions, without a PAT or signing-key service.

From a reviewed checkout containing the intended version, tag and push the release:

```sh
ORQOS_TAG=v0.2.2  # use the version recorded in Cargo.toml and Cargo.lock
git tag "$ORQOS_TAG"
git push origin "$ORQOS_TAG"
```

To retry, open **Actions → Native Ubuntu release (draft) → Run workflow** and enter the exact existing tag. The workflow checks out that tag. Wait for both architecture builds and the release job, review the draft's `.deb` packages, retained binaries, manifest, checksums and attestations, and inspect installation/lifecycle logs. Publish manually only after review; the workflow refuses to modify a published release. RawPair's compatibility manifest pins `v0.2.2`, so publish that partner before building RawPair's paired release. See [RawPair's release procedure](https://github.com/rawpair/rawpair/blob/main/docs/RELEASING.md).

For local packaging after `cargo build --release --locked`:

```sh
python3 packaging/build.py --tag v0.2.2 --arch amd64
python3 -m unittest discover -s packaging -p 'test_*.py' -v
```

Checksums verify download integrity, not signatures. Optional provenance verification is separate:

```sh
gh attestation verify orqos-host_VERSION_ARCH.deb --repo orqos-project/orqos
```

RawPair's compatibility manifest requires the `v0.2.2` native-package release. Publish that reviewed Orqos release before building the paired RawPair release. Implementing the workflow does not publish release assets; a maintainer must complete the process above. Live systemd/network checks require a disposable privileged Linux host; source/unit tests alone do not establish a running installation.

Workspace homes start empty, with only the owned project and local-tool directories. Provisioning does not copy the host’s `/etc/skel`; install any desired shell configuration deliberately.
