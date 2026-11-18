//! Renders a pod spec into Quadlet unit files. Pure functions: the output depends only on the pod ID, its
//! ownership token, the validated spec and its [`Inputs`] (where secret files live, and the secrets' values).
//!
//! Quoting follows what was verified against Quadlet 5.7 and systemd 259: `command` (plus `args`) travels as a
//! JSON `Entrypoint=`, which carries any argument intact; bare `args` use systemd command-line quoting (empty and
//! lone ";" arguments are rejected by validation); environment values go through `EnvironmentFile=`, which
//! Podman reads literally. `%` and `$` are escaped wherever systemd would expand them.
use std::path::Path;

use super::files::{self, Secrets};
use super::spec::{ContainerSpec, Medium, PodSpec, Restart};

/// What rendering needs beyond the spec: the root of secret files and the values of the secrets the pod uses
/// (verified to exist, and to be UTF-8 without control characters where they become environment variables).
pub struct Inputs<'a> {
    pub files: &'a Path,
    pub secrets: &'a Secrets,
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
    for container in &spec.containers {
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
            content: render_container(id, token, spec, container, &base, inputs.files),
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
    c: &ContainerSpec,
    base: &str,
    files_root: &Path,
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
    for m in &c.mounts {
        let volume = spec
            .volumes
            .iter()
            .find(|v| v.name == m.volume)
            .expect("validated: mounts name pod volumes");
        if volume.secret.is_some() {
            // Root-owned files, read-only; idmapped so their IDs inside match those on disk.
            let source = files::source(files_root, id, &volume.name, m.sub_path.as_deref());
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
    let restart = match spec.restart {
        Restart::Always => "always",
        Restart::OnFailure => "on-failure",
        Restart::Never => "no",
    };
    format!(
        "{header}[Unit]\nDescription={description}\nStartLimitIntervalSec=0\n\n{container}\n\
         [Service]\nRestart={restart}\nRestartSec=1s\nRestartSteps=8\nRestartMaxDelaySec=300s\n\
         TimeoutStartSec=600\nTimeoutStopSec={stop}\n",
        header = header(token),
        description = description(id, token, Some(&c.name)),
        stop = spec.grace_seconds + 30,
    )
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
    fn unit_names_cannot_collide_with_the_pod_unit() {
        assert_eq!(pod_unit("shop"), "orqos-shop-pod.service");
        assert_eq!(container_unit("shop", "pod"), "orqos-shop-c-pod.service");
    }
}
