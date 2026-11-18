//! Renders a pod spec into Quadlet unit files. Pure functions: the output depends only on the pod ID, its
//! ownership token and the validated spec.
//!
//! Quoting follows what was verified against Quadlet 5.7 and systemd 259: `command` (plus `args`) travels as a
//! JSON `Entrypoint=`, which carries any argument intact; bare `args` use systemd command-line quoting (empty and
//! lone ";" arguments are rejected by validation); environment values go through `EnvironmentFile=`, which
//! Podman reads literally. `%` and `$` are escaped wherever systemd would expand them.
use super::spec::{ContainerSpec, PodSpec, Restart};

pub const OWNER_LABEL: &str = "dev.orqos.owner";
pub const POD_LABEL: &str = "dev.orqos.pod";
pub const NETWORK_LABEL: &str = "dev.orqos.network";

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

pub fn render(id: &str, token: &str, spec: &PodSpec) -> Vec<File> {
    let mut files = vec![File {
        name: format!("{}.pod", pod_name(id)),
        content: render_pod(id, token, spec),
    }];
    for container in &spec.containers {
        let base = format!("orqos-{id}-c-{}", container.name);
        if !container.env.is_empty() {
            files.push(File {
                name: format!("{base}.env"),
                content: container
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}\n"))
                    .collect(),
            });
        }
        files.push(File {
            name: format!("{base}.container"),
            content: render_container(id, token, spec, container, &base),
        });
    }
    files
}

fn render_pod(id: &str, token: &str, spec: &PodSpec) -> String {
    format!(
        "{header}[Unit]\nDescription={description}\nStartLimitIntervalSec=0\n\n\
         [Pod]\nPodName={pod}\nHostName={hostname}\nUserNS=auto\n{network}\
         Label={OWNER_LABEL}={token}\nLabel={POD_LABEL}={id}\n{labels}ExitPolicy=continue\nStopTimeout={grace}\n\n\
         [Service]\nTimeoutStopSec={stop}\n",
        header = header(token),
        description = description(id, token, None),
        pod = pod_name(id),
        hostname = spec.hostname,
        network = network(spec),
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
    if !c.env.is_empty() {
        container.push_str(&format!("EnvironmentFile={base}.env\n"));
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
        let files = render("shop", TOKEN, &spec);
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
    fn unit_names_cannot_collide_with_the_pod_unit() {
        assert_eq!(pod_unit("shop"), "orqos-shop-pod.service");
        assert_eq!(container_unit("shop", "pod"), "orqos-shop-c-pod.service");
    }
}
