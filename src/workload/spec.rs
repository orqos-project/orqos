//! The pod specification accepted by `PUT /workload/pods/{id}`. Orqos-native: it does not depend on Rezn's IR.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A pod of containers sharing one network namespace and hostname, run as owned Quadlet units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PodSpec {
    /// Hostname inside every container of the pod (DNS label, at most 63 characters).
    pub hostname: String,
    pub containers: Vec<ContainerSpec>,
    /// Run once, in order, before sidecars and containers start; each must succeed before the next. A failure is
    /// retried under the pod's restart policy (never: the pod start fails).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub init: Vec<ContainerSpec>,
    /// Started after the init containers and before `containers`, always restarted, stopped after `containers`.
    /// When a sidecar has a startup probe, `containers` start only once every such probe has passed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<ContainerSpec>,
    /// Restart policy of every container. Restarts back off from 1 s to 5 minutes.
    #[serde(default)]
    pub restart: Restart,
    /// Seconds between SIGTERM and SIGKILL when a container stops.
    #[serde(default = "default_grace")]
    pub grace_seconds: u32,
    /// Labels set on the Podman pod and its containers, for clients' own bookkeeping (for example Rezn's
    /// ownership). Keys are lowercase `[a-z0-9./-]`; the `dev.orqos.` prefix is reserved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// An Orqos-owned Podman network to join instead of Podman's default network, with DNS aliases for the pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSpec>,
    /// Extra group IDs of every container (`GroupAdd=`). The first also owns the pod's scratch volumes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supplementary_groups: Vec<u32>,
    /// Volumes the pod's containers can mount.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<PodVolume>,
}

/// A pod volume: exactly one of `volume` (an owned volume, `PUT /workload/volumes/{name}`), `scratch` or `secret`
/// (files from an owned secret, `PUT /workload/secrets/{name}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PodVolume {
    /// DNS label, unique in the pod; mounts refer to it.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch: Option<Scratch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretFiles>,
}

/// Keys of an owned secret written as files when the pod starts, mounted read-only. The volume replaces whatever
/// the image has at the mount path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretFiles {
    /// The owned secret.
    pub name: String,
    /// Keys to write and their relative paths; without it, every key under its own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<SecretItem>>,
    /// Mode of every file (at most 0777); directories get `r-x` wherever files get `r`.
    #[serde(default = "default_mode")]
    pub mode: u32,
    /// Group of every file and directory (their owner is root); `root` without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretItem {
    pub key: String,
    /// Relative path of the file inside the volume.
    pub path: String,
}

/// One key of an owned secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretKeyRef {
    pub secret: String,
    pub key: String,
}

fn default_mode() -> u32 {
    0o644
}

/// The spec of an owned secret (`PUT /workload/secrets/{name}`): named data whose values are never returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretSpec {
    /// Values by key, base64-encoded. Immutable once created: changed data needs a new secret.
    pub data: BTreeMap<String, String>,
    /// The client's own labels, kept in Orqos's record (updatable).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// Decoded secret data may total at most 1 MiB, like a Kubernetes Secret.
pub const MAX_SECRET_BYTES: usize = 1 << 20;

impl SecretSpec {
    /// Checks the spec and returns the decoded values.
    pub fn decode(&self) -> Result<BTreeMap<String, Vec<u8>>, String> {
        use base64::Engine;
        if self.data.len() > 256 {
            return Err("data: at most 256 keys".into());
        }
        let mut decoded = BTreeMap::new();
        let mut total = 0;
        for (key, value) in &self.data {
            if !valid_secret_key(key) {
                return Err(format!(
                    "data: invalid key {key:?} (letters, digits, '-', '_' and '.', at most 253; not \".\" or \"..\")"
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value)
                .map_err(|_| format!("data.{key}: not standard base64"))?;
            total += bytes.len();
            decoded.insert(key.clone(), bytes);
        }
        if total > MAX_SECRET_BYTES {
            return Err(format!("data: at most {MAX_SECRET_BYTES} bytes in total"));
        }
        validate_labels(&self.labels)?;
        Ok(decoded)
    }
}

/// Owned secret names, like volume names.
pub fn valid_secret_name(name: &str) -> bool {
    is_label(name, 48)
}

/// Kubernetes' key grammar: `[-._a-zA-Z0-9]{1,253}`, not `.` or `..`. Keys are also file names.
pub fn valid_secret_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 253
        && key != "."
        && key != ".."
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

/// A relative file path of plain components (no empty, `.` or `..` components).
fn valid_item_path(path: &str) -> bool {
    valid_path(path, false)
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Empty when the pod is created or replaced, shared by its containers, removed with the pod.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Scratch {
    pub medium: Medium,
    /// Size limit: enforced for `memory` (tmpfs), advisory for `disk`.
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Medium {
    Disk,
    Memory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MountSpec {
    /// A pod volume name.
    pub volume: String,
    /// Absolute path in the container.
    pub path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// Relative path inside the volume to mount instead of its root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_path: Option<String>,
}

/// The spec of an owned volume (`PUT /workload/volumes/{name}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// Group owning the volume's root directory (mode 2775, setgid); without it the root is `root:root` 0755.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    /// The client's own labels, kept in Orqos's record (updatable), never on the Podman volume.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl SecretFiles {
    fn validate(&self, at: &str) -> Result<(), String> {
        if !valid_secret_name(&self.name) {
            return Err(format!("{at}.name: a DNS label of at most 48 characters"));
        }
        if self.mode > 0o777 {
            return Err(format!("{at}.mode: at most 0777 (511)"));
        }
        if self.group.is_some_and(|g| g > MAX_ID) {
            return Err(format!("{at}.group: at most {MAX_ID}"));
        }
        let Some(items) = &self.items else {
            return Ok(());
        };
        if items.is_empty() || items.len() > 256 {
            return Err(format!("{at}.items: omit it or list 1 to 256 items"));
        }
        let mut paths: BTreeSet<&str> = BTreeSet::new();
        for (k, item) in items.iter().enumerate() {
            if !valid_secret_key(&item.key) {
                return Err(format!("{at}.items[{k}].key: invalid key"));
            }
            if !valid_item_path(&item.path) {
                return Err(format!(
                    "{at}.items[{k}].path: a relative path of letters, digits and / . _ - + @, without empty, '.' or '..' components"
                ));
            }
            // A path cannot be both a file and a directory of another item.
            if paths.iter().any(|p| {
                *p == item.path
                    || p.starts_with(&format!("{}/", item.path))
                    || item.path.starts_with(&format!("{p}/"))
            }) {
                return Err(format!("{at}.items[{k}].path: conflicts with another item"));
            }
            paths.insert(&item.path);
        }
        Ok(())
    }

    /// Each projected file's key and relative path, given the secret's keys.
    pub fn files<'a>(&'a self, keys: impl Iterator<Item = &'a str>) -> Vec<(&'a str, &'a str)> {
        match &self.items {
            Some(items) => items
                .iter()
                .map(|i| (i.key.as_str(), i.path.as_str()))
                .collect(),
            None => keys.map(|k| (k, k)).collect(),
        }
    }
}

/// Where a container sits in its pod's lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    Init,
    Sidecar,
    Container,
}

impl ContainerKind {
    /// The spec field listing containers of this kind.
    pub fn field(self) -> &'static str {
        match self {
            ContainerKind::Init => "init",
            ContainerKind::Sidecar => "sidecars",
            ContainerKind::Container => "containers",
        }
    }
}

/// User, group and supplementary IDs must fall inside the pod's 65536-ID user namespace.
pub const MAX_ID: u32 = 65535;

impl VolumeSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.group.is_some_and(|g| g > MAX_ID) {
            return Err(format!("group: at most {MAX_ID}"));
        }
        validate_labels(&self.labels)
    }
}

/// Owned volume names become `orqos-vol-<name>`.
pub fn valid_volume_name(name: &str) -> bool {
    is_label(name, 48)
}

/// Paths travel through Quadlet's comma-separated `Mount=`: a conservative character set, no `..` components.
fn valid_path(path: &str, absolute: bool) -> bool {
    path.len() <= 1024
        && path.starts_with('/') == absolute
        && !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+@".contains(&b))
        && !path.split('/').any(|c| c == "..")
}

/// Pods naming the same network share it and resolve each other's aliases (aardvark-dns). Orqos creates the
/// network on first use and removes it when no pod names it any more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// DNS label of at most 40 characters; the Podman network is `orqos-net-<name>`.
    pub name: String,
    /// DNS names that resolve to this pod on the network, in the given order. Several pods may share an alias:
    /// it then resolves to all of them.
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn default_grace() -> u32 {
    30
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Restart {
    #[default]
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    /// DNS label, unique in the pod.
    pub name: String,
    /// Image reference; pulled when missing.
    pub image: String,
    /// Replaces the image's entrypoint (and its default command), like Kubernetes `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Replaces the image's default command, like Kubernetes `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Environment variables, passed literally.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variables from owned secrets: values are UTF-8 without control characters. Names are disjoint
    /// from `env`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, SecretKeyRef>,
    /// UID to run as (inside the pod's user namespace); the image's user when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<u32>,
    /// Primary GID; needs `user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only_root: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_new_privileges: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drop_all_capabilities: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountSpec>,
    /// Until it succeeds, no other probe runs and the container is not started; reaching its failure threshold
    /// restarts the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<Probe>,
    /// Decides whether the container is ready; never restarts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Probe>,
    /// Reaching its failure threshold restarts the container (under the pod's restart policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Probe>,
}

/// A probe Orqos runs every `period_seconds`: `exec` inside the container, or an HTTP(S) `GET` from outside it,
/// inside the pod's network namespace (so images need no HTTP client).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    /// Command run with `podman exec` as the container's user; exit status 0 succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpProbe>,
    pub period_seconds: u32,
    pub timeout_seconds: u32,
    /// Consecutive failures that make the probe fail.
    pub failure_threshold: u32,
}

/// `GET http(s)://127.0.0.1:<port><path>` in the pod's network namespace. Statuses 200–399 succeed; `https` does not
/// verify the certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpProbe {
    pub path: String,
    pub port: u16,
    pub scheme: Scheme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Http,
    Https,
}

impl Probe {
    fn validate(&self, at: &str) -> Result<(), String> {
        match (&self.exec, &self.http) {
            (Some(argv), None) => {
                if argv.is_empty() || argv.len() > 64 {
                    return Err(format!("{at}.exec: 1 to 64 arguments"));
                }
                if argv.iter().any(|a| has_control(a)) {
                    return Err(format!("{at}.exec: control characters are not allowed"));
                }
            }
            (None, Some(http)) => {
                let path_ok = http.path.starts_with('/')
                    && http.path.len() <= 1024
                    && http
                        .path
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?%".contains(&b));
                if !path_ok {
                    return Err(format!(
                        "{at}.http.path: an absolute path of URI characters, at most 1024"
                    ));
                }
                if http.port == 0 {
                    return Err(format!("{at}.http.port: 1 to 65535"));
                }
            }
            _ => return Err(format!("{at}: exactly one of exec or http")),
        }
        if !(1..=3600).contains(&self.period_seconds) {
            return Err(format!("{at}.period_seconds: 1 to 3600"));
        }
        // As in Kubernetes, a timeout may exceed the period: the next run waits for the current one.
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err(format!("{at}.timeout_seconds: 1 to 300"));
        }
        if !(1..=1000).contains(&self.failure_threshold) {
            return Err(format!("{at}.failure_threshold: 1 to 1000"));
        }
        Ok(())
    }
}

/// Pod IDs become unit, pod and container names, so they are shorter than DNS labels (as in host mode).
pub fn valid_id(id: &str) -> bool {
    is_label(id, 48)
}

fn is_label(value: &str, max: usize) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Network names become `orqos-net-<name>`; 40 characters keep that within Podman's limits with room to spare.
pub fn valid_network_name(name: &str) -> bool {
    is_label(name, 40)
}

/// A lowercase DNS name: dot-separated labels of at most 63 characters, at most 253 in total.
fn valid_dns_name(name: &str) -> bool {
    name.len() <= 253 && name.split('.').all(|label| is_label(label, 63))
}

fn valid_label_key(key: &str) -> bool {
    key.len() <= 128
        && key
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"./-".contains(&b))
        && !key.starts_with("dev.orqos.")
}

/// Label values need no quoting in Quadlet `Label=` lines and no systemd escaping.
fn valid_label_value(value: &str) -> bool {
    value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
}

fn has_control(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

fn valid_image(image: &str) -> bool {
    !image.is_empty()
        && image.len() <= 255
        && image
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
        && image.as_bytes()[0].is_ascii_alphanumeric()
}

fn valid_env_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), String> {
    if labels.len() > 32 {
        return Err("labels: at most 32".into());
    }
    for (key, value) in labels {
        if !valid_label_key(key) {
            return Err(format!(
                "labels: invalid key {key:?} (lowercase letters, digits, '.', '/' and '-', at most 128; \"dev.orqos.\" is reserved)"
            ));
        }
        if !valid_label_value(value) {
            return Err(format!(
                "labels.{key}: letters, digits and . _ : / @ + -, at most 256 characters"
            ));
        }
    }
    Ok(())
}

impl PodSpec {
    /// Names of the owned volumes the pod mounts.
    pub fn owned_volumes(&self) -> impl Iterator<Item = &str> {
        self.volumes.iter().filter_map(|v| v.volume.as_deref())
    }

    /// Every container with its kind: init containers in order, then sidecars, then containers.
    pub fn every_container(&self) -> impl Iterator<Item = (ContainerKind, &ContainerSpec)> {
        self.init
            .iter()
            .map(|c| (ContainerKind::Init, c))
            .chain(self.sidecars.iter().map(|c| (ContainerKind::Sidecar, c)))
            .chain(
                self.containers
                    .iter()
                    .map(|c| (ContainerKind::Container, c)),
            )
    }

    /// Whether some sidecar has a startup probe, which then gates the start of `containers`.
    pub fn gated(&self) -> bool {
        self.sidecars.iter().any(|c| c.startup.is_some())
    }

    /// Names of the owned secrets the pod uses, as files or environment variables.
    pub fn secrets(&self) -> BTreeSet<&str> {
        self.volumes
            .iter()
            .filter_map(|v| v.secret.as_ref().map(|s| s.name.as_str()))
            .chain(
                self.every_container()
                    .flat_map(|(_, c)| c.secret_env.values().map(|r| r.secret.as_str())),
            )
            .collect()
    }

    /// The pod's secret volumes.
    pub fn secret_files(&self) -> impl Iterator<Item = (&str, &SecretFiles)> {
        self.volumes
            .iter()
            .filter_map(|v| Some((v.name.as_str(), v.secret.as_ref()?)))
    }

    /// The pod's scratch volumes.
    pub fn scratch(&self) -> impl Iterator<Item = (&str, &Scratch)> {
        self.volumes
            .iter()
            .filter_map(|v| Some((v.name.as_str(), v.scratch.as_ref()?)))
    }

    /// Checks the spec, returning the first problem with its field path.
    pub fn validate(&self) -> Result<(), String> {
        if !is_label(&self.hostname, 63) {
            return Err(
                "hostname: a DNS label of at most 63 lowercase letters, digits or '-'".into(),
            );
        }
        if self.containers.is_empty() {
            return Err("containers: at least one container".into());
        }
        if self.grace_seconds > 3600 {
            return Err("grace_seconds: at most 3600".into());
        }
        validate_labels(&self.labels)?;
        if self.supplementary_groups.len() > 16 {
            return Err("supplementary_groups: at most 16".into());
        }
        if self.supplementary_groups.iter().any(|g| *g > MAX_ID) {
            return Err(format!("supplementary_groups: IDs at most {MAX_ID}"));
        }
        if self.volumes.len() > 32 {
            return Err("volumes: at most 32".into());
        }
        let mut volumes = BTreeSet::new();
        for (i, v) in self.volumes.iter().enumerate() {
            let at = format!("volumes[{i}]");
            if !is_label(&v.name, 63) {
                return Err(format!("{at}.name: a DNS label of at most 63 characters"));
            }
            if !volumes.insert(v.name.as_str()) {
                return Err(format!("{at}.name: duplicate volume name {:?}", v.name));
            }
            match (&v.volume, &v.scratch, &v.secret) {
                (Some(name), None, None) if valid_volume_name(name) => {}
                (Some(_), None, None) => {
                    return Err(format!("{at}.volume: a DNS label of at most 48 characters"))
                }
                (None, Some(scratch), None) if scratch.bytes > 0 => {}
                (None, Some(_), None) => return Err(format!("{at}.scratch.bytes: at least 1")),
                (None, None, Some(files)) => files.validate(&format!("{at}.secret"))?,
                _ => return Err(format!("{at}: exactly one of volume, scratch or secret")),
            }
        }
        if let Some(network) = &self.network {
            if !valid_network_name(&network.name) {
                return Err("network.name: a DNS label of at most 40 characters".into());
            }
            if network.aliases.len() > 64 {
                return Err("network.aliases: at most 64".into());
            }
            let mut seen = BTreeSet::new();
            for (i, alias) in network.aliases.iter().enumerate() {
                if !valid_dns_name(alias) {
                    return Err(format!(
                        "network.aliases[{i}]: a lowercase DNS name (dot-separated labels, at most 253 characters)"
                    ));
                }
                if !seen.insert(alias.as_str()) {
                    return Err(format!("network.aliases[{i}]: duplicate alias {alias:?}"));
                }
            }
        }
        if self.init.len() + self.sidecars.len() + self.containers.len() > 32 {
            return Err("at most 32 containers in all".into());
        }
        let mut names = BTreeSet::new();
        let mut index: BTreeMap<ContainerKind, usize> = BTreeMap::new();
        for (kind, c) in self.every_container() {
            let i = index.entry(kind).or_default();
            let at = format!("{}[{i}]", kind.field());
            *i += 1;
            if kind == ContainerKind::Init
                && (c.startup.is_some() || c.readiness.is_some() || c.liveness.is_some())
            {
                return Err(format!(
                    "{at}: init containers have no probes (they run to completion)"
                ));
            }
            if !is_label(&c.name, 63) {
                return Err(format!("{at}.name: a DNS label of at most 63 characters"));
            }
            if !names.insert(c.name.as_str()) {
                return Err(format!("{at}.name: duplicate container name {:?}", c.name));
            }
            if !valid_image(&c.image) {
                return Err(format!(
                    "{at}.image: letters, digits and . _ - / : @, at most 255 characters"
                ));
            }
            if let Some(command) = &c.command {
                if command.is_empty() {
                    return Err(format!(
                        "{at}.command: omit it or give at least one element"
                    ));
                }
                if command.iter().any(|a| has_control(a)) {
                    return Err(format!("{at}.command: control characters are not allowed"));
                }
            }
            if let Some(args) = &c.args {
                if args.iter().any(|a| has_control(a)) {
                    return Err(format!("{at}.args: control characters are not allowed"));
                }
                // Without `command`, arguments travel through systemd command-line parsing, where an empty word
                // ends the list and a lone ";" separates commands.
                if c.command.is_none() && args.iter().any(|a| a.is_empty() || a == ";") {
                    return Err(format!(
                        "{at}.args: empty or \";\" arguments need `command` (e.g. the image's entrypoint) to be set"
                    ));
                }
            }
            if c.user.is_some_and(|u| u > MAX_ID) || c.group.is_some_and(|g| g > MAX_ID) {
                return Err(format!("{at}: user and group IDs at most {MAX_ID}"));
            }
            if c.group.is_some() && c.user.is_none() {
                return Err(format!("{at}.group: needs user"));
            }
            if c.mounts.len() > 32 {
                return Err(format!("{at}.mounts: at most 32"));
            }
            let mut paths = BTreeSet::new();
            for (k, m) in c.mounts.iter().enumerate() {
                let here = format!("{at}.mounts[{k}]");
                if !volumes.contains(m.volume.as_str()) {
                    return Err(format!("{here}.volume: no pod volume {:?}", m.volume));
                }
                if !valid_path(&m.path, true) || m.path == "/" {
                    return Err(format!(
                        "{here}.path: an absolute path of letters, digits and / . _ - + @, without '..'"
                    ));
                }
                if !paths.insert(m.path.trim_end_matches('/')) {
                    return Err(format!("{here}.path: mounted twice"));
                }
                if m.sub_path.as_deref().is_some_and(|p| !valid_path(p, false)) {
                    return Err(format!(
                        "{here}.sub_path: a relative path of letters, digits and / . _ - + @, without '..'"
                    ));
                }
            }
            for (key, value) in &c.env {
                if !valid_env_key(key) {
                    return Err(format!("{at}.env: invalid variable name {key:?}"));
                }
                if has_control(value) {
                    return Err(format!(
                        "{at}.env.{key}: control characters are not allowed"
                    ));
                }
            }
            for (kind, probe) in [
                ("startup", &c.startup),
                ("readiness", &c.readiness),
                ("liveness", &c.liveness),
            ] {
                if let Some(probe) = probe {
                    probe.validate(&format!("{at}.{kind}"))?;
                }
            }
            if c.env.len() + c.secret_env.len() > 256 {
                return Err(format!("{at}: at most 256 environment variables"));
            }
            for (key, reference) in &c.secret_env {
                if !valid_env_key(key) {
                    return Err(format!("{at}.secret_env: invalid variable name {key:?}"));
                }
                if c.env.contains_key(key) {
                    return Err(format!("{at}.secret_env.{key}: also set in env"));
                }
                if !valid_secret_name(&reference.secret) {
                    return Err(format!(
                        "{at}.secret_env.{key}.secret: a DNS label of at most 48 characters"
                    ));
                }
                if !valid_secret_key(&reference.key) {
                    return Err(format!("{at}.secret_env.{key}.key: invalid key"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(value: serde_json::Value) -> Result<PodSpec, String> {
        let spec: PodSpec = serde_json::from_value(value).map_err(|e| e.to_string())?;
        spec.validate().map(|()| spec)
    }

    fn minimal() -> serde_json::Value {
        json!({"hostname": "web-0", "containers": [{"name": "web", "image": "docker.io/library/nginx:1.27"}]})
    }

    #[test]
    fn defaults_and_minimal_spec() {
        let s = spec(minimal()).unwrap();
        assert_eq!(s.restart, Restart::Always);
        assert_eq!(s.grace_seconds, 30);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        let mut v = minimal();
        v["privileged"] = json!(true);
        assert!(spec(v).is_err());
        let mut v = minimal();
        v["hostname"] = json!("Web");
        assert!(spec(v).unwrap_err().starts_with("hostname"));
        let mut v = minimal();
        v["containers"][0]["image"] = json!("nginx latest");
        assert!(spec(v).unwrap_err().contains(".image"));
        let mut v = minimal();
        v["containers"][0]["env"] = json!({"A": "line\nbreak"});
        assert!(spec(v).unwrap_err().contains("control"));
        let mut v = minimal();
        v["containers"][0]["env"] = json!({"1A": "x"});
        assert!(spec(v).unwrap_err().contains("variable name"));
        let mut v = minimal();
        v["containers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "web", "image": "alpine"}));
        assert!(spec(v).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn labels_are_checked() {
        let mut v = minimal();
        v["labels"] = json!({"dev.rezn.owner": "ab12", "dev.rezn.ordinal": "0", "empty": ""});
        assert!(spec(v.clone()).is_ok());
        v["labels"] = json!({"dev.orqos.owner": "x"});
        assert!(spec(v.clone()).unwrap_err().contains("reserved"));
        v["labels"] = json!({"Upper": "x"});
        assert!(spec(v.clone()).unwrap_err().contains("invalid key"));
        v["labels"] = json!({"k": "two words"});
        assert!(spec(v.clone()).unwrap_err().starts_with("labels.k"));
        v["labels"] = json!({"k": "100%"});
        assert!(spec(v).unwrap_err().starts_with("labels.k"));
        let mut v = minimal();
        v["containers"][0]["name"] = json!("c".repeat(63));
        assert!(spec(v.clone()).is_ok());
        v["containers"][0]["name"] = json!("c".repeat(64));
        assert!(spec(v).is_err());
    }

    #[test]
    fn networks_are_checked() {
        let mut v = minimal();
        v["network"] = json!({"name": "rz-0123", "aliases": ["web", "web.shop.svc", "web-0.web"]});
        assert_eq!(spec(v.clone()).unwrap().network.unwrap().aliases.len(), 3);
        v["network"] = json!({"name": "rz-0123"});
        assert!(spec(v.clone()).unwrap().network.unwrap().aliases.is_empty());
        v["network"] = json!({"name": "n".repeat(41)});
        assert!(spec(v.clone()).unwrap_err().starts_with("network.name"));
        v["network"] = json!({"name": "Net"});
        assert!(spec(v.clone()).unwrap_err().starts_with("network.name"));
        for bad in [
            "Web", "web..svc", ".web", "web.", "-web", "web_1", "a b", "",
        ] {
            v["network"] = json!({"name": "n", "aliases": [bad]});
            assert!(
                spec(v.clone())
                    .unwrap_err()
                    .starts_with("network.aliases[0]"),
                "{bad:?}"
            );
        }
        v["network"] = json!({"name": "n", "aliases": ["web", "web"]});
        assert!(spec(v.clone()).unwrap_err().contains("duplicate"));
        v["network"] =
            json!({"name": "n", "aliases": (0..65).map(|i| format!("a{i}")).collect::<Vec<_>>()});
        assert!(spec(v.clone()).unwrap_err().contains("at most 64"));
        v["network"] = json!({"name": "n", "extra": 1});
        assert!(spec(v).is_err());
    }

    #[test]
    fn identity_volumes_and_mounts_are_checked() {
        let mut v = minimal();
        v["supplementary_groups"] = json!([2000]);
        v["volumes"] = json!([
            {"name": "state", "volume": "rz-0123"},
            {"name": "tmp", "scratch": {"medium": "memory", "bytes": 1048576}}
        ]);
        v["containers"][0]["user"] = json!(1000);
        v["containers"][0]["group"] = json!(1000);
        v["containers"][0]["read_only_root"] = json!(true);
        v["containers"][0]["no_new_privileges"] = json!(true);
        v["containers"][0]["drop_all_capabilities"] = json!(true);
        v["containers"][0]["mounts"] = json!([
            {"volume": "state", "path": "/var/lib/app", "sub_path": "data/v1"},
            {"volume": "tmp", "path": "/tmp", "read_only": false},
            {"volume": "state", "path": "/etc/app", "read_only": true}
        ]);
        let ok = spec(v.clone()).unwrap();
        assert_eq!(ok.owned_volumes().collect::<Vec<_>>(), ["rz-0123"]);
        assert_eq!(ok.scratch().count(), 1);

        let broken = |path: &str, value: serde_json::Value| {
            let mut b = v.clone();
            let mut target = &mut b;
            for key in path.split('.') {
                target = match key.parse::<usize>() {
                    Ok(i) => &mut target[i],
                    Err(_) => &mut target[key],
                };
            }
            *target = value;
            spec(b).unwrap_err()
        };
        assert!(broken("containers.0.mounts.0.volume", json!("nope")).contains("no pod volume"));
        assert!(broken("containers.0.mounts.0.path", json!("relative")).contains(".path"));
        assert!(broken("containers.0.mounts.0.path", json!("/a/../b")).contains(".path"));
        assert!(broken("containers.0.mounts.0.path", json!("/a,b")).contains(".path"));
        assert!(broken("containers.0.mounts.0.path", json!("/tmp")).contains("mounted twice"));
        assert!(broken("containers.0.mounts.0.sub_path", json!("/abs")).contains("sub_path"));
        assert!(broken("containers.0.mounts.0.sub_path", json!("a/../../b")).contains("sub_path"));
        assert!(broken("containers.0.user", json!(65536)).contains("at most 65535"));
        assert!(broken("supplementary_groups", json!([70000])).contains("at most 65535"));
        assert!(broken("volumes.1.scratch.bytes", json!(0)).contains("at least 1"));
        assert!(broken("volumes.1", json!({"name": "tmp"})).contains("exactly one"));
        assert!(broken("volumes.1", json!({"name": "state", "volume": "x"})).contains("duplicate"));
        assert!(broken("volumes.0.volume", json!("Bad")).contains("48"));
        let mut g = v.clone();
        g["containers"][0].as_object_mut().unwrap().remove("user");
        assert!(spec(g).unwrap_err().contains("needs user"));
    }

    #[test]
    fn secret_volumes_and_environment_are_checked() {
        let mut v = minimal();
        v["volumes"] = json!([
            {"name": "tls", "secret": {"name": "rz-tls", "mode": 288, "group": 2000}},
            {"name": "conf", "secret": {"name": "rz-conf", "items": [
                {"key": "settings.json", "path": "settings.json"},
                {"key": "ca.crt", "path": "certs/ca.pem"}
            ]}}
        ]);
        v["containers"][0]["env"] = json!({"LEVEL": "info"});
        v["containers"][0]["secret_env"] =
            json!({"DB_PASSWORD": {"secret": "rz-db", "key": "password"}});
        v["containers"][0]["mounts"] = json!([{"volume": "tls", "path": "/etc/tls"}]);
        let ok = spec(v.clone()).unwrap();
        assert_eq!(
            ok.secrets().into_iter().collect::<Vec<_>>(),
            ["rz-conf", "rz-db", "rz-tls"]
        );
        assert_eq!(ok.volumes[1].secret.as_ref().unwrap().mode, 0o644);
        assert_eq!(ok.secret_files().count(), 2);

        let broken = |pointer: &str, value: serde_json::Value| {
            let mut b = v.clone();
            *b.pointer_mut(pointer).unwrap() = value;
            spec(b).unwrap_err()
        };
        assert!(broken("/volumes/0/secret/mode", json!(512)).contains("0777"));
        assert!(broken("/volumes/0/secret/group", json!(65536)).contains("65535"));
        assert!(broken("/volumes/0/secret/name", json!("Bad")).contains("48"));
        assert!(broken("/volumes/1/secret/items", json!([])).contains("1 to 256"));
        for bad in ["/abs", "a/../b", "./a", "a//b", "a/", "a,b", ""] {
            assert!(
                broken("/volumes/1/secret/items/0/path", json!(bad)).contains("relative path"),
                "{bad:?}"
            );
        }
        assert!(broken("/volumes/1/secret/items/0/path", json!("certs")).contains("conflicts"));
        assert!(
            broken("/volumes/1/secret/items/0/path", json!("certs/ca.pem")).contains("conflicts")
        );
        assert!(broken("/volumes/1/secret/items/0/key", json!("..")).contains("invalid key"));
        assert!(broken("/volumes/1/secret/items/0/key", json!("a/b")).contains("invalid key"));
        assert!(broken(
            "/volumes/0",
            json!({"name": "tls", "volume": "v", "secret": {"name": "s"}})
        )
        .contains("exactly one"));
        assert!(broken(
            "/containers/0/secret_env",
            json!({"LEVEL": {"secret": "s", "key": "k"}})
        )
        .contains("also set in env"));
        assert!(broken(
            "/containers/0/secret_env",
            json!({"1X": {"secret": "s", "key": "k"}})
        )
        .contains("variable name"));
        assert!(broken(
            "/containers/0/secret_env",
            json!({"X": {"secret": "S", "key": "k"}})
        )
        .contains(".secret"));
        assert!(broken(
            "/containers/0/secret_env",
            json!({"X": {"secret": "s", "key": "."}})
        )
        .contains(".key"));
    }

    #[test]
    fn probes_are_checked() {
        let mut v = minimal();
        v["containers"][0]["startup"] = json!({"exec": ["test", "-f", "/tmp/started"], "period_seconds": 5, "timeout_seconds": 1, "failure_threshold": 60});
        v["containers"][0]["readiness"] = json!({"http": {"path": "/health/ready?full=1", "port": 8080, "scheme": "https"}, "period_seconds": 10, "timeout_seconds": 3, "failure_threshold": 3});
        v["containers"][0]["liveness"] = json!({"http": {"path": "/", "port": 80, "scheme": "http"}, "period_seconds": 10, "timeout_seconds": 1, "failure_threshold": 3});
        let ok = spec(v.clone()).unwrap();
        assert_eq!(
            ok.containers[0]
                .readiness
                .as_ref()
                .unwrap()
                .http
                .as_ref()
                .unwrap()
                .scheme,
            Scheme::Https
        );
        let broken = |pointer: &str, value: serde_json::Value| {
            let mut b = v.clone();
            *b.pointer_mut(pointer).unwrap() = value;
            spec(b).unwrap_err()
        };
        assert!(broken("/containers/0/startup/exec", json!([])).contains("1 to 64"));
        assert!(broken("/containers/0/startup/exec", json!(["a\nb"])).contains("control"));
        assert!(broken("/containers/0/readiness/http/path", json!("health")).contains(".path"));
        assert!(broken("/containers/0/readiness/http/path", json!("/a b")).contains(".path"));
        assert!(broken("/containers/0/readiness/http/port", json!(0)).contains("1 to 65535"));
        assert!(
            broken("/containers/0/liveness/period_seconds", json!(0)).contains("period_seconds")
        );
        assert!(broken("/containers/0/liveness/timeout_seconds", json!(301)).contains("1 to 300"));
        let mut long = v.clone();
        long["containers"][0]["liveness"]["timeout_seconds"] = json!(30);
        assert!(spec(long).is_ok(), "a timeout may exceed the period");
        assert!(broken("/containers/0/liveness/failure_threshold", json!(0))
            .contains("failure_threshold"));
        assert!(broken("/containers/0/liveness", json!({"exec": ["true"], "http": {"path": "/", "port": 1, "scheme": "http"}, "period_seconds": 1, "timeout_seconds": 1, "failure_threshold": 1})).contains("exactly one"));
        // Every field is explicit: no defaults to drift between client and server.
        let mut missing = v.clone();
        missing["containers"][0]["liveness"]
            .as_object_mut()
            .unwrap()
            .remove("timeout_seconds");
        assert!(spec(missing).is_err());
    }

    #[test]
    fn init_containers_and_sidecars_are_checked() {
        let mut v = minimal();
        v["init"] = json!([{"name": "prepare", "image": "alpine:3.22"}, {"name": "trust", "image": "alpine:3.22"}]);
        v["sidecars"] = json!([{"name": "credentials", "image": "alpine:3.22",
            "startup": {"exec": ["test", "-s", "/run/token"], "period_seconds": 5, "timeout_seconds": 1, "failure_threshold": 120},
            "secret_env": {"TOKEN": {"secret": "rz-app", "key": "token"}}}]);
        let ok = spec(v.clone()).unwrap();
        let order: Vec<(ContainerKind, &str)> = ok
            .every_container()
            .map(|(k, c)| (k, c.name.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                (ContainerKind::Init, "prepare"),
                (ContainerKind::Init, "trust"),
                (ContainerKind::Sidecar, "credentials"),
                (ContainerKind::Container, "web"),
            ]
        );
        assert!(ok.gated());
        assert!(ok.secrets().contains("rz-app"), "sidecar secrets count");
        let broken = |pointer: &str, value: serde_json::Value| {
            let mut b = v.clone();
            *b.pointer_mut(pointer).unwrap() = value;
            spec(b).unwrap_err()
        };
        assert!(broken("/init/1/name", json!("web")).contains("duplicate container name"));
        assert!(broken("/sidecars/0/name", json!("prepare")).starts_with("sidecars[0].name"));
        assert!(broken("/init/0/image", json!("bad image")).starts_with("init[0].image"));
        let probe = json!({"exec": ["true"], "period_seconds": 1, "timeout_seconds": 1, "failure_threshold": 1});
        let mut probed = v.clone();
        probed["init"][0]["readiness"] = probe;
        assert!(spec(probed)
            .unwrap_err()
            .contains("init containers have no probes"));
        let mut many = v.clone();
        many["containers"] = json!((0..31)
            .map(|i| json!({"name": format!("c{i}"), "image": "alpine"}))
            .collect::<Vec<_>>());
        assert!(spec(many).unwrap_err().contains("at most 32 containers"));
        let mut plain = minimal();
        plain["sidecars"] = json!([{"name": "log", "image": "alpine"}]);
        assert!(
            !spec(plain).unwrap().gated(),
            "only a sidecar startup probe gates"
        );
    }

    #[test]
    fn secret_specs_are_checked_and_decoded() {
        let ok: SecretSpec = serde_json::from_value(
            json!({"data": {"tls.crt": "Q0VSVA==", ".dockerconfigjson": ""}, "labels": {"dev.rezn.kind": "secret"}}),
        )
        .unwrap();
        let decoded = ok.decode().unwrap();
        assert_eq!(decoded["tls.crt"], b"CERT");
        assert_eq!(decoded[".dockerconfigjson"], b"");
        let check = |data: serde_json::Value| {
            serde_json::from_value::<SecretSpec>(json!({"data": data}))
                .unwrap()
                .decode()
                .unwrap_err()
        };
        assert!(check(json!({"k": "not base64!"})).contains("base64"));
        assert!(check(json!({"..": ""})).contains("invalid key"));
        assert!(check(json!({"a b": ""})).contains("invalid key"));
        use base64::Engine;
        let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_SECRET_BYTES + 1]);
        assert!(check(json!({"k": big})).contains("in total"));
        assert!(serde_json::from_value::<SecretSpec>(json!({"data": {}, "type": "tls"})).is_err());
        assert!(valid_secret_key(&"k".repeat(253)));
        assert!(!valid_secret_key(&"k".repeat(254)));
    }

    #[test]
    fn volume_specs_are_checked() {
        let ok: VolumeSpec =
            serde_json::from_value(json!({"group": 2000, "labels": {"dev.rezn.retain": "true"}}))
                .unwrap();
        assert!(ok.validate().is_ok());
        let big: VolumeSpec = serde_json::from_value(json!({"group": 65536})).unwrap();
        assert!(big.validate().is_err());
        let reserved: VolumeSpec =
            serde_json::from_value(json!({"labels": {"dev.orqos.x": "1"}})).unwrap();
        assert!(reserved.validate().unwrap_err().contains("reserved"));
        assert!(serde_json::from_value::<VolumeSpec>(json!({"size": 1})).is_err());
        assert!(valid_volume_name("rz-0123456789abcdef01234567"));
        assert!(!valid_volume_name(&"a".repeat(49)));
    }

    #[test]
    fn args_needing_command_are_explained() {
        let mut v = minimal();
        v["containers"][0]["args"] = json!(["a", ""]);
        assert!(spec(v.clone()).unwrap_err().contains("need `command`"));
        v["containers"][0]["args"] = json!([";"]);
        assert!(spec(v.clone()).unwrap_err().contains("need `command`"));
        v["containers"][0]["command"] = json!(["/bin/sh", "-c"]);
        assert!(spec(v).is_ok());
    }

    #[test]
    fn ids_are_short_labels() {
        assert!(valid_id("shop-api-0"));
        assert!(!valid_id(&"a".repeat(49)));
        assert!(!valid_id("Shop"));
        assert!(!valid_id("-x"));
    }
}
