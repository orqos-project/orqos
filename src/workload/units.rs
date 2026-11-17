//! systemd and Podman interaction for owned workload units: command execution, `systemctl show` parsing,
//! ownership checks, and atomic installation of a pod's unit directory.
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::render::{self, File, OWNER_LABEL};
use super::WorkloadError;

const PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
const MARKER: &str = ".orqos-owner";

#[derive(Clone, Debug)]
pub struct Paths {
    /// Scanned by the Quadlet system generator; one subdirectory per pod, named by its token.
    pub units: PathBuf,
    /// Same filesystem as `units` but not scanned: render, validate, then rename into place.
    pub staging: PathBuf,
    /// Pod records.
    pub state: PathBuf,
    pub quadlet: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            units: "/etc/containers/systemd/orqos".into(),
            staging: "/etc/containers/orqos-staging".into(),
            state: "/var/lib/orqos/workload".into(),
            quadlet: "/usr/libexec/podman/quadlet".into(),
        }
    }
}

/// Runs a program with a cleared environment and a fixed PATH. Non-zero exits become backend errors that name
/// the command and its stderr.
pub async fn run(
    program: &str,
    args: &[&str],
    env: &[(&str, &Path)],
    timeout: Duration,
) -> Result<String, WorkloadError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", PATH)
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let shown = format!("{program} {}", args.join(" "));
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            WorkloadError::Backend(format!("{shown}: timed out after {}s", timeout.as_secs()))
        })?
        .map_err(|e| WorkloadError::Backend(format!("{shown}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr
            .lines()
            .filter(|l| !l.contains("Loading source unit file"))
            .collect();
        return Err(WorkloadError::Backend(format!(
            "{shown}: {}",
            lines.join(" | ").trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

const SHORT: Duration = Duration::from_secs(30);
const START: Duration = Duration::from_secs(660);

pub fn parse_show(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub async fn show(unit: &str) -> Result<BTreeMap<String, String>, WorkloadError> {
    let text = run(
        "systemctl",
        &[
            "show",
            unit,
            "--no-pager",
            "--property=LoadState,ActiveState,SubState,Result,NRestarts,SourcePath,Description",
        ],
        &[],
        SHORT,
    )
    .await?;
    Ok(parse_show(&text))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ownership {
    Absent,
    Owned,
    Foreign(String),
}

/// A generated unit belongs to a pod when systemd knows it from a file inside the pod's token directory and its
/// description names the pod's token.
pub fn unit_ownership(show: &BTreeMap<String, String>, token: &str, dir: &Path) -> Ownership {
    if show.get("LoadState").map(String::as_str) == Some("not-found") {
        return Ownership::Absent;
    }
    let source = show
        .get("SourcePath")
        .map(String::as_str)
        .unwrap_or_default();
    let description = show
        .get("Description")
        .map(String::as_str)
        .unwrap_or_default();
    if Path::new(source).starts_with(dir) && description.ends_with(&format!(" owner {token}")) {
        Ownership::Owned
    } else {
        Ownership::Foreign(format!("source {source:?}, description {description:?}"))
    }
}

/// Labels of a Podman pod, or `None` when no pod of that name exists.
pub async fn pod_labels(name: &str) -> Result<Option<BTreeMap<String, String>>, WorkloadError> {
    let exists = run("podman", &["pod", "exists", name], &[], SHORT)
        .await
        .is_ok();
    if !exists {
        return Ok(None);
    }
    let text = run(
        "podman",
        &["pod", "inspect", name, "--format", "json"],
        &[],
        SHORT,
    )
    .await?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| WorkloadError::Backend(format!("podman pod inspect: {e}")))?;
    let pod = if value.is_array() { &value[0] } else { &value };
    Ok(Some(
        pod["Labels"]
            .as_object()
            .map(|labels| {
                labels
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    ))
}

pub fn pod_ownership(labels: Option<&BTreeMap<String, String>>, token: &str) -> Ownership {
    match labels {
        None => Ownership::Absent,
        Some(labels) if labels.get(OWNER_LABEL).map(String::as_str) == Some(token) => {
            Ownership::Owned
        }
        Some(labels) => Ownership::Foreign(format!("owner label {:?}", labels.get(OWNER_LABEL))),
    }
}

fn io(context: &str, path: &Path) -> impl Fn(std::io::Error) -> WorkloadError {
    let what = format!("{context} {}", path.display());
    move |e| WorkloadError::Backend(format!("{what}: {e}"))
}

fn read_marker(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(MARKER))
        .ok()
        .map(|s| s.trim().to_string())
}

fn write_files(dir: &Path, token: &str, files: &[File]) -> Result<(), WorkloadError> {
    fs::create_dir_all(dir).map_err(io("create", dir))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io("chmod", dir))?;
    let mut all = files.to_vec();
    all.push(File {
        name: MARKER.into(),
        content: format!("{token}\n"),
    });
    for file in &all {
        let path = dir.join(&file.name);
        let mut handle = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(io("create", &path))?;
        std::io::Write::write_all(&mut handle, file.content.as_bytes())
            .map_err(io("write", &path))?;
        handle.sync_all().map_err(io("sync", &path))?;
    }
    Ok(())
}

/// Renders into staging, has Quadlet validate the result, then swaps it into the scanned directory and reloads
/// systemd. An existing directory is replaced only if its marker holds the same token.
pub async fn install(
    paths: &Paths,
    id: &str,
    token: &str,
    files: &[File],
) -> Result<(), WorkloadError> {
    let staged = paths.staging.join(token);
    let old = paths.staging.join(format!("{token}.old"));
    let target = paths.units.join(token);
    for dir in [&paths.staging, &paths.units] {
        fs::create_dir_all(dir).map_err(io("create", dir))?;
    }
    for leftover in [&staged, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).map_err(io("remove", leftover))?;
        }
    }
    write_files(&staged, token, files)?;
    let quadlet = paths.quadlet.to_string_lossy().into_owned();
    if let Err(error) = run(
        &quadlet,
        &["-dryrun"],
        &[("QUADLET_UNIT_DIRS", &staged)],
        SHORT,
    )
    .await
    {
        let _ = fs::remove_dir_all(&staged);
        return Err(WorkloadError::Invalid(format!(
            "Quadlet rejected the rendered units for {id}: {error}"
        )));
    }
    if target.exists() {
        if read_marker(&target).as_deref() != Some(token) {
            return Err(WorkloadError::Conflict(format!(
                "{} is not owned by pod {id}",
                target.display()
            )));
        }
        fs::rename(&target, &old).map_err(io("move", &target))?;
    }
    fs::rename(&staged, &target).map_err(io("move", &staged))?;
    if old.exists() {
        fs::remove_dir_all(&old).map_err(io("remove", &old))?;
    }
    daemon_reload().await
}

/// Removes a pod's unit directory (only with a matching marker) and reloads systemd.
pub async fn uninstall(paths: &Paths, id: &str, token: &str) -> Result<(), WorkloadError> {
    let target = paths.units.join(token);
    if target.exists() {
        if read_marker(&target).as_deref() != Some(token) {
            return Err(WorkloadError::Conflict(format!(
                "{} is not owned by pod {id}",
                target.display()
            )));
        }
        fs::remove_dir_all(&target).map_err(io("remove", &target))?;
    }
    daemon_reload().await
}

pub async fn daemon_reload() -> Result<(), WorkloadError> {
    run("systemctl", &["daemon-reload"], &[], SHORT)
        .await
        .map(|_| ())
}

pub async fn systemctl(verb: &str, unit: &str) -> Result<(), WorkloadError> {
    let timeout = if verb == "start" || verb == "restart" {
        START
    } else {
        SHORT * 4
    };
    run("systemctl", &[verb, unit], &[], timeout)
        .await
        .map(|_| ())
}

pub async fn reset_failed(units: &[String]) {
    for unit in units {
        let _ = run("systemctl", &["reset-failed", unit], &[], SHORT).await;
    }
}

/// Every unit of a pod: the pod service first, then its containers.
pub fn units_of(id: &str, containers: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    std::iter::once(render::pod_unit(id))
        .chain(
            containers
                .into_iter()
                .map(|c| render::container_unit(id, c.as_ref())),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn shown(load: &str, source: &str, description: &str) -> BTreeMap<String, String> {
        parse_show(&format!("LoadState={load}\nActiveState=active\nSourcePath={source}\nDescription={description}\n"))
    }

    #[test]
    fn show_output_parses_key_values() {
        let map = parse_show("A=1\nB=x=y\n\nnoise\n");
        assert_eq!(map["A"], "1");
        assert_eq!(map["B"], "x=y");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn units_are_owned_only_from_their_token_directory_and_description() {
        let dir = Path::new("/etc/containers/systemd/orqos").join(TOKEN);
        let owned = shown(
            "loaded",
            &format!("{}/orqos-shop.pod", dir.display()),
            &format!("Orqos workload pod shop owner {TOKEN}"),
        );
        assert_eq!(unit_ownership(&owned, TOKEN, &dir), Ownership::Owned);
        assert_eq!(
            unit_ownership(&shown("not-found", "", ""), TOKEN, &dir),
            Ownership::Absent
        );
        let elsewhere = shown(
            "loaded",
            "/etc/containers/systemd/shop.pod",
            &format!("Orqos workload pod shop owner {TOKEN}"),
        );
        assert!(matches!(
            unit_ownership(&elsewhere, TOKEN, &dir),
            Ownership::Foreign(_)
        ));
        let other_token = shown(
            "loaded",
            &format!("{}/x.pod", dir.display()),
            "Orqos workload pod shop owner ffff",
        );
        assert!(matches!(
            unit_ownership(&other_token, TOKEN, &dir),
            Ownership::Foreign(_)
        ));
        let prefix = Path::new("/etc/containers/systemd/orqos").join(format!("{TOKEN}x"));
        let sibling = shown(
            "loaded",
            &format!("{}/x.pod", prefix.display()),
            &format!("pod owner {TOKEN}"),
        );
        assert!(matches!(
            unit_ownership(&sibling, TOKEN, &dir),
            Ownership::Foreign(_)
        ));
    }

    #[test]
    fn pods_are_owned_by_their_label() {
        let mut labels = BTreeMap::new();
        assert_eq!(pod_ownership(None, TOKEN), Ownership::Absent);
        assert!(matches!(
            pod_ownership(Some(&labels), TOKEN),
            Ownership::Foreign(_)
        ));
        labels.insert(OWNER_LABEL.to_string(), TOKEN.to_string());
        assert_eq!(pod_ownership(Some(&labels), TOKEN), Ownership::Owned);
    }

    #[test]
    fn unit_list_starts_with_the_pod() {
        assert_eq!(
            units_of("shop", ["web", "worker"]),
            vec![
                "orqos-shop-pod.service",
                "orqos-shop-c-web.service",
                "orqos-shop-c-worker.service"
            ]
        );
    }
}
