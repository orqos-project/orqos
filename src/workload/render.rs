//! Renders a pod spec into Quadlet unit files. Pure functions: the output depends only on the pod ID, its
//! ownership token, the validated spec and its [`Inputs`] (where secret files live, and the secrets' values).
//!
//! Quoting follows what was verified against Quadlet 5.7 and systemd 259: `command` (plus `args`) travels as a
//! JSON `Entrypoint=`, which carries any argument intact; bare `args` use systemd command-line quoting (empty and
//! lone ";" arguments are rejected by validation); environment values go through `EnvironmentFile=`, which
//! Podman reads literally. `%` and `$` are escaped wherever systemd would expand them.
use std::path::Path;

use super::files::{self, Secrets};
use super::spec::{ContainerKind, ContainerSpec, Medium, PodSpec, Resources, Restart};

/// What rendering needs beyond the spec: the root of secret files and the values of the secrets the pod uses
/// (verified to exist, and to be UTF-8 without control characters where they become environment variables).
pub struct Inputs<'a> {
    pub files: &'a Path,
    pub secrets: &'a Secrets,
    /// The host's CPUs × 1000: Podman refuses a CPU limit above them, so limits are capped there.
    pub cpu_millis: u64,
}

/// Quadlet lines for a container's cgroup settings. Without swap, as in Kubernetes; CPU requests become shares
/// (millis × 1024 / 1000, at least 2, which cgroup v2 turns into `cpu.weight`).
pub fn resource_lines(r: &Resources, host_cpu_millis: u64) -> String {
    let mut lines = String::new();
    if let Some(bytes) = r.memory_limit_bytes {
        lines.push_str(&format!(
            "Memory={bytes}\nPodmanArgs=--memory-swap={bytes}\n"
        ));
    }
    if let Some(bytes) = r.memory_request_bytes {
        lines.push_str(&format!("PodmanArgs=--memory-reservation={bytes}\n"));
    }
    if let Some(millis) = r.cpu_limit_millis {
        let millis = millis.min(host_cpu_millis.max(1));
        lines.push_str(&format!(
            "PodmanArgs=--cpus={}.{:03}\n",
            millis / 1000,
            millis % 1000
        ));
    }
    if let Some(millis) = r.cpu_request_millis {
        lines.push_str(&format!(
            "PodmanArgs=--cpu-shares={}\n",
            (millis * 1024 / 1000).max(2)
        ));
    }
    lines
}

pub const OWNER_LABEL: &str = "dev.orqos.owner";
pub const POD_LABEL: &str = "dev.orqos.pod";
pub const NETWORK_LABEL: &str = "dev.orqos.network";
pub const VOLUME_LABEL: &str = "dev.orqos.volume";

/// Every pod gets its own range of 65536 IDs, so user, group and supplementary IDs up to 65535 are mapped.
pub const USERNS: &str = "auto:size=65536";

/// One rendered file: a name inside the pod's unit directory and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub content: String,
}

pub fn pod_name(id: &str) -> String {
    format!("orqos-{id}")
}

/// The Podman network of a network spec name.
pub fn network_name(name: &str) -> String {
    format!("orqos-net-{name}")
}

/// The Podman volume of an owned volume.
pub fn volume_name(name: &str) -> String {
    format!("orqos-vol-{name}")
}

/// The Podman volume backing one scratch volume of a pod.
pub fn scratch_name(id: &str, volume: &str) -> String {
    format!("orqos-{id}-scratch-{volume}")
}

/// Where a memory scratch volume is also mounted at pod level, so the infra container keeps the tmpfs (and its
/// contents) for the pod's lifetime even while no other container runs.
fn scratch_hold(volume: &str) -> String {
    format!("/run/orqos-scratch/{volume}")
}

/// The file whose existence lets a gated pod's containers start: created once every sidecar with a startup probe
/// has passed it. On tmpfs, so a reboot closes every gate.
pub fn gate_path(id: &str) -> std::path::PathBuf {
    std::path::Path::new("/run/orqos/workload")
        .join(id)
        .join("sidecars-started")
}

/// The systemd service Quadlet generates for the pod.
pub fn pod_unit(id: &str) -> String {
    format!("orqos-{id}-pod.service")
}

/// The systemd service Quadlet generates for one container.
pub fn container_unit(id: &str, container: &str) -> String {
    format!("orqos-{id}-c-{container}.service")
}

/// The `Description=` of every generated unit names its pod and owner; ownership checks rely on it.
pub fn description(id: &str, token: &str, container: Option<&str>) -> String {
    match container {
        Some(name) => format!("Orqos workload pod {id} container {name} owner {token}"),
        None => format!("Orqos workload pod {id} owner {token}"),
    }
}

/// Escapes `%` (unit specifiers) and `$` (variable expansion) for systemd.
fn systemd_literal(value: &str) -> String {
    value.replace('%', "%%").replace('$', "$$")
}

/// One double-quoted systemd command-line word.
fn systemd_word(value: &str) -> String {
    format!(
        "\"{}\"",
        systemd_literal(&value.replace('\\', "\\\\").replace('"', "\\\""))
    )
}

/// `Label=` lines for the client's labels (validated to need no quoting), after Orqos's own.
fn client_labels(spec: &PodSpec) -> String {
    spec.labels
        .iter()
        .map(|(k, v)| format!("Label={k}={v}\n"))
        .collect()
}

/// `Network=` and `NetworkAlias=` lines (aliases are validated DNS names, which need no quoting).
fn network(spec: &PodSpec) -> String {
    let Some(network) = &spec.network else {
        return String::new();
    };
    let mut lines = format!("Network={}\n", network_name(&network.name));
    for alias in &network.aliases {
        lines.push_str(&format!("NetworkAlias={alias}\n"));
    }
    lines
}

fn header(token: &str) -> String {
    format!(
        "# Managed by Orqos for owner {token}. Do not edit: the next apply replaces this file.\n"
    )
}

pub fn render(id: &str, token: &str, spec: &PodSpec, inputs: &Inputs) -> Vec<File> {
    let mut files = vec![File {
        name: format!("{}.pod", pod_name(id)),
        content: render_pod(id, token, spec),
    }];
    for (kind, container) in spec.every_container() {
        let base = format!("orqos-{id}-c-{}", container.name);
        if !container.env.is_empty() || !container.secret_env.is_empty() {
            let secret_env = container.secret_env.iter().map(|(k, r)| {
                let value = inputs
                    .secrets
                    .get(&r.secret)
                    .and_then(|data| data.get(&r.key))
                    .map(|v| String::from_utf8_lossy(v).into_owned())
                    .unwrap_or_default();
                (k, value)
            });
            let mut lines: Vec<(&String, String)> = container
                .env
                .iter()
                .map(|(k, v)| (k, v.clone()))
                .chain(secret_env)
                .collect();
            lines.sort();
            files.push(File {
                name: format!("{base}.env"),
                content: lines.iter().map(|(k, v)| format!("{k}={v}\n")).collect(),
            });
        }
        files.push(File {
            name: format!("{base}.container"),
            content: render_container(id, token, spec, kind, container, &base, inputs),
        });
    }
    files
}

fn render_pod(id: &str, token: &str, spec: &PodSpec) -> String {
    format!(
        "{header}[Unit]\nDescription={description}\nStartLimitIntervalSec=0\n\n\
         [Pod]\nPodName={pod}\nHostName={hostname}\nUserNS={USERNS}\n{network}{holds}\
         Label={OWNER_LABEL}={token}\nLabel={POD_LABEL}={id}\n{labels}ExitPolicy=continue\nStopTimeout={grace}\n\n\
         [Service]\nTimeoutStopSec={stop}\n",
        header = header(token),
        description = description(id, token, None),
        pod = pod_name(id),
        hostname = spec.hostname,
        network = network(spec),
        holds = spec
            .scratch()
            .filter(|(_, s)| s.medium == Medium::Memory)
            .map(|(name, _)| format!(
                "Volume={}:{}:idmap\n",
                scratch_name(id, name),
                scratch_hold(name)
            ))
            .collect::<String>(),
        labels = client_labels(spec),
        grace = spec.grace_seconds,
        stop = spec.grace_seconds + 30,
    )
}

fn render_container(
    id: &str,
    token: &str,
    spec: &PodSpec,
    kind: ContainerKind,
    c: &ContainerSpec,
    base: &str,
    inputs: &Inputs,
) -> String {
    let mut container = format!(
        "[Container]\nContainerName={pod}-{name}\nPod={pod}.pod\nImage={image}\n",
        pod = pod_name(id),
        name = c.name,
        image = c.image,
    );
    match (&c.command, &c.args) {
        (Some(command), args) => {
            let argv: Vec<&String> = command.iter().chain(args.iter().flatten()).collect();
            let json = serde_json::to_string(&argv).expect("strings serialize");
            container.push_str(&format!("Entrypoint={}\n", systemd_literal(&json)));
        }
        (None, Some(args)) if !args.is_empty() => {
            let words: Vec<String> = args.iter().map(|a| systemd_word(a)).collect();
            container.push_str(&format!("Exec={}\n", words.join(" ")));
        }
        _ => {}
    }
    if !c.env.is_empty() || !c.secret_env.is_empty() {
        container.push_str(&format!("EnvironmentFile={base}.env\n"));
    }
    match (c.user, c.group) {
        (Some(user), Some(group)) => container.push_str(&format!("User={user}\nGroup={group}\n")),
        (Some(user), None) => container.push_str(&format!("User={user}\n")),
        _ => {}
    }
    for group in &spec.supplementary_groups {
        container.push_str(&format!("GroupAdd={group}\n"));
    }
    if c.read_only_root {
        container.push_str("ReadOnly=true\n");
    }
    if c.no_new_privileges {
        container.push_str("NoNewPrivileges=true\n");
    }
    if c.drop_all_capabilities {
        container.push_str("DropCapability=all\n");
    }
    if let Some(resources) = &c.resources {
        container.push_str(&resource_lines(resources, inputs.cpu_millis));
    }
    for m in &c.mounts {
        let volume = spec
            .volumes
            .iter()
            .find(|v| v.name == m.volume)
            .expect("validated: mounts name pod volumes");
        if volume.secret.is_some() {
            // Root-owned files, read-only; idmapped so their IDs inside match those on disk.
            let source = files::source(inputs.files, id, &volume.name, m.sub_path.as_deref());
            container.push_str(&format!(
                "Mount=type=bind,source={},destination={},ro,idmap\n",
                source.display(),
                m.path
            ));
            continue;
        }
        let source = match &volume.volume {
            Some(owned) => volume_name(owned),
            None => scratch_name(id, &volume.name),
        };
        // Idmapped: on disk, IDs stay those seen in the container whatever range the pod's namespace gets.
        let mut line = format!(
            "Mount=type=volume,source={source},destination={},idmap",
            m.path
        );
        if let Some(sub) = &m.sub_path {
            line.push_str(&format!(",subpath={sub}"));
        }
        if m.read_only {
            line.push_str(",ro");
        }
        container.push_str(&line);
        container.push('\n');
    }
    container.push_str(&format!(
        "Label={OWNER_LABEL}={token}\nLabel={POD_LABEL}={id}\n{}StopTimeout={}\n",
        client_labels(spec),
        spec.grace_seconds
    ));
    let (unit, service) = lifecycle(id, spec, kind, c);
    format!(
        "{header}[Unit]\nDescription={description}\nStartLimitIntervalSec=0\n{unit}\n{container}\n\
         [Service]\n{service}RestartSec=1s\nRestartSteps=8\nRestartMaxDelaySec=300s\n\
         TimeoutStopSec={stop}\n",
        header = header(token),
        description = description(id, token, Some(&c.name)),
        stop = spec.grace_seconds + 30,
    )
}

/// A container's place in the pod's start order, as systemd `[Unit]` dependencies and `[Service]` settings:
///
/// - init containers are one-shot units that run in order, each `Requires=` the previous one; a failed one is
///   retried in place (`RestartMode=direct`, so units waiting on it keep waiting) unless the pod never restarts;
/// - sidecars and containers require the last init container; sidecars always restart;
/// - containers start after the sidecars (`Wants=`, so a sidecar crash does not stop them, and they stop first),
///   and wait at an `ExecStartPre` gate while the pod has sidecars with startup probes that have not all passed.
fn lifecycle(id: &str, spec: &PodSpec, kind: ContainerKind, c: &ContainerSpec) -> (String, String) {
    let requires = |name: &str| {
        let unit = container_unit(id, name);
        format!("After={unit}\nRequires={unit}\n")
    };
    let policy = match spec.restart {
        Restart::Always => "always",
        Restart::OnFailure => "on-failure",
        Restart::Never => "no",
    };
    match kind {
        ContainerKind::Init => {
            let position = spec.init.iter().position(|i| i.name == c.name).unwrap_or(0);
            let unit = match position {
                0 => String::new(),
                n => requires(&spec.init[n - 1].name),
            };
            let restart = if spec.restart == Restart::Never {
                "no"
            } else {
                "on-failure"
            };
            (
                unit,
                format!(
                    "Type=oneshot\nRemainAfterExit=yes\nRestart={restart}\nRestartMode=direct\nTimeoutStartSec=infinity\n"
                ),
            )
        }
        ContainerKind::Sidecar => (
            spec.init
                .last()
                .map(|i| requires(&i.name))
                .unwrap_or_default(),
            "Restart=always\nTimeoutStartSec=600\n".into(),
        ),
        ContainerKind::Container => {
            let mut unit = spec
                .init
                .last()
                .map(|i| requires(&i.name))
                .unwrap_or_default();
            for sidecar in &spec.sidecars {
                let name = container_unit(id, &sidecar.name);
                unit.push_str(&format!("After={name}\nWants={name}\n"));
            }
            let service = if spec.gated() {
                format!(
                    "ExecStartPre=/bin/sh -c 'until test -e {}; do sleep 1; done'\nRestart={policy}\nTimeoutStartSec=infinity\n",
                    gate_path(id).display()
                )
            } else {
                format!("Restart={policy}\nTimeoutStartSec=600\n")
            };
            (unit, service)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn golden(case: &str, spec: serde_json::Value) {
        let spec: PodSpec = serde_json::from_value(spec).unwrap();
        spec.validate().unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/workload")
            .join(case);
        let secrets: Secrets = serde_json::from_value(serde_json::json!({
            "rz-tls": {"tls.crt": b"CERT".to_vec(), "tls.key": b"KEY".to_vec()},
            "rz-db": {"password": b"p a$$ \"w%rd\" #1".to_vec()}
        }))
        .unwrap();
        let inputs = Inputs {
            files: Path::new("/var/lib/orqos/workload/files"),
            secrets: &secrets,
            cpu_millis: 4000,
        };
        let files = render("shop", TOKEN, &spec, &inputs);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            for file in &files {
                std::fs::write(dir.join(&file.name), &file.content).unwrap();
            }
        }
        let mut expected: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        expected.sort();
        let mut names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(names, expected, "{case}: rendered file set");
        for file in files {
            let want = std::fs::read_to_string(dir.join(&file.name)).unwrap();
            assert_eq!(file.content, want, "{case}/{}", file.name);
        }
    }

    #[test]
    fn renders_a_two_container_pod() {
        golden(
            "two-containers",
            serde_json::json!({
                "hostname": "shop-0",
                "grace_seconds": 40,
                "containers": [
                    {"name": "web", "image": "docker.io/library/nginx:1.27-alpine"},
                    {"name": "worker", "image": "registry.example.com/shop/worker@sha256:0000000000000000000000000000000000000000000000000000000000000000",
                     "args": ["--mode", "document", "--grace", "25"],
                     "env": {"LEVEL": "info", "QUOTED": "a \"b\" $HOME 100%"}}
                ]
            }),
        );
    }

    #[test]
    fn renders_commands_and_hard_arguments() {
        golden(
            "quoting",
            serde_json::json!({
                "hostname": "q",
                "restart": "on_failure",
                "labels": {"dev.rezn.owner": "0f0f", "dev.rezn.resource": "queue", "app.example/tier": "batch"},
                "containers": [
                    {"name": "entry", "image": "alpine:3.22", "command": ["/bin/sh", "-c"],
                     "args": ["echo \"$1\" 100%", "sh", "", ";", "back\\slash"]},
                    {"name": "bare", "image": "alpine:3.22", "args": ["a b", "quote\"d", "$X", "50%"]}
                ]
            }),
        );
    }

    #[test]
    fn renders_a_pod_on_a_network() {
        golden(
            "network",
            serde_json::json!({
                "hostname": "db-0",
                "network": {"name": "rz-0123456789abcdef01234567", "aliases": ["db", "db.shop", "db.shop.svc", "db-0.db", "db-0.db.shop", "db-0.db.shop.svc"]},
                "containers": [{"name": "db", "image": "docker.io/library/postgres:17-alpine"}]
            }),
        );
    }

    #[test]
    fn renders_identity_hardening_and_volumes() {
        golden(
            "volumes",
            serde_json::json!({
                "hostname": "db-0",
                "supplementary_groups": [2000],
                "volumes": [
                    {"name": "data", "volume": "rz-0123456789abcdef01234567"},
                    {"name": "tmp", "scratch": {"medium": "memory", "bytes": 1048576}},
                    {"name": "cache", "scratch": {"medium": "disk", "bytes": 1073741824}}
                ],
                "containers": [{
                    "name": "db", "image": "docker.io/library/postgres:17-alpine",
                    "user": 70, "group": 70, "read_only_root": true, "no_new_privileges": true,
                    "drop_all_capabilities": true,
                    "mounts": [
                        {"volume": "data", "path": "/var/lib/postgresql/data", "sub_path": "pgdata"},
                        {"volume": "tmp", "path": "/tmp"},
                        {"volume": "cache", "path": "/cache"},
                        {"volume": "data", "path": "/backup", "read_only": true}
                    ]
                }]
            }),
        );
    }

    #[test]
    fn renders_secret_files_and_environment() {
        golden(
            "secrets",
            serde_json::json!({
                "hostname": "api-0",
                "supplementary_groups": [2000],
                "volumes": [
                    {"name": "tls", "secret": {"name": "rz-tls", "mode": 288, "group": 2000}},
                    {"name": "settings", "secret": {"name": "rz-tls", "items": [{"key": "tls.crt", "path": "certs/ca.pem"}]}}
                ],
                "containers": [{
                    "name": "api", "image": "registry.example.com/api:1",
                    "user": 1000, "read_only_root": true,
                    "env": {"LEVEL": "info"},
                    "secret_env": {"DB_PASSWORD": {"secret": "rz-db", "key": "password"}},
                    "mounts": [
                        {"volume": "tls", "path": "/etc/tls"},
                        {"volume": "settings", "path": "/etc/app/ca.pem", "sub_path": "certs/ca.pem"},
                        {"volume": "settings", "path": "/etc/certs", "read_only": false}
                    ]
                }]
            }),
        );
    }

    #[test]
    fn renders_init_containers_and_gated_sidecars() {
        let probe = serde_json::json!({"exec": ["test", "-s", "/work/token"], "period_seconds": 5,
                                       "timeout_seconds": 1, "failure_threshold": 120});
        golden(
            "init-and-sidecars",
            serde_json::json!({
                "hostname": "worker-0",
                "volumes": [{"name": "work", "scratch": {"medium": "memory", "bytes": 1048576}}],
                "init": [
                    {"name": "trust", "image": "alpine:3.22", "command": ["/bin/sh", "-c", "echo 1 > /work/order"],
                     "mounts": [{"volume": "work", "path": "/work"}]},
                    {"name": "prepare", "image": "alpine:3.22", "command": ["/bin/sh", "-c", "echo 2 >> /work/order"],
                     "mounts": [{"volume": "work", "path": "/work"}]}
                ],
                "sidecars": [{"name": "credentials", "image": "alpine:3.22", "startup": probe,
                              "mounts": [{"volume": "work", "path": "/work"}]}],
                "containers": [{"name": "worker", "image": "alpine:3.22",
                                "mounts": [{"volume": "work", "path": "/work"}]}]
            }),
        );
        let never: PodSpec = serde_json::from_value(serde_json::json!({
            "hostname": "job-0", "restart": "never",
            "init": [{"name": "wait", "image": "alpine:3.22"}],
            "containers": [{"name": "job", "image": "alpine:3.22"}]
        }))
        .unwrap();
        never.validate().unwrap();
        let secrets = Secrets::new();
        let inputs = Inputs {
            files: Path::new("/var/lib/orqos/workload/files"),
            secrets: &secrets,
            cpu_millis: 4000,
        };
        let files = render("job", TOKEN, &never, &inputs);
        let init = &files
            .iter()
            .find(|f| f.name == "orqos-job-c-wait.container")
            .unwrap()
            .content;
        assert!(
            init.contains("Type=oneshot\nRemainAfterExit=yes\nRestart=no\n"),
            "{init}"
        );
        let main = &files
            .iter()
            .find(|f| f.name == "orqos-job-c-job.container")
            .unwrap()
            .content;
        assert!(
            main.contains("After=orqos-job-c-wait.service\nRequires=orqos-job-c-wait.service\n")
        );
        assert!(
            !main.contains("ExecStartPre"),
            "no sidecar startup probe, no gate"
        );
    }

    #[test]
    fn resources_become_cgroup_settings() {
        golden(
            "resources",
            serde_json::json!({
                "hostname": "api-0",
                "containers": [{"name": "api", "image": "alpine:3.22", "resources": {
                    "cpu_limit_millis": 1500, "cpu_request_millis": 250,
                    "memory_limit_bytes": 67108864, "memory_request_bytes": 33554432}}]
            }),
        );
        let r = Resources {
            cpu_limit_millis: Some(8000),
            cpu_request_millis: Some(1),
            ..Default::default()
        };
        assert_eq!(
            resource_lines(&r, 2000),
            "PodmanArgs=--cpus=2.000\nPodmanArgs=--cpu-shares=2\n",
            "limits above the host's CPUs are capped; shares are at least 2"
        );
        assert_eq!(resource_lines(&Resources::default(), 2000), "");
    }

    #[test]
    fn unit_names_cannot_collide_with_the_pod_unit() {
        assert_eq!(pod_unit("shop"), "orqos-shop-pod.service");
        assert_eq!(container_unit("shop", "pod"), "orqos-shop-c-pod.service");
    }
}
