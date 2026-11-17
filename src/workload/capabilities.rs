//! Host checks for the workload backend (orqos#16): root, systemd ≥ 254, cgroup v2, Podman ≥ 5.0 with its
//! Quadlet system generator, and subordinate ID ranges for `UserNS=auto`.
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use utoipa::ToSchema;

use super::units::{run, Paths};

pub const MIN_SYSTEMD: u32 = 254;
pub const MIN_PODMAN: (u32, u32) = (5, 0);
const GENERATOR: &str = "/usr/lib/systemd/system-generators/podman-system-generator";

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Capabilities {
    pub root: bool,
    pub systemd_version: Option<u32>,
    pub cgroup_v2: bool,
    pub podman_version: Option<String>,
    pub quadlet: bool,
    /// `/etc/subuid` and `/etc/subgid` give the `containers` user ranges, which `UserNS=auto` allocates from.
    pub subordinate_ids: bool,
    /// Empty when every requirement is met.
    pub problems: Vec<String>,
}

pub fn parse_systemd_version(text: &str) -> Option<u32> {
    text.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

pub fn parse_podman_version(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value["Client"]["Version"].as_str().map(str::to_string)
}

pub fn version_at_least(version: &str, minimum: (u32, u32)) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor) >= minimum
}

pub fn has_range(file_contents: &str, user: &str) -> bool {
    file_contents.lines().any(|line| {
        let mut fields = line.split(':');
        fields.next() == Some(user)
            && fields
                .next()
                .is_some_and(|start| start.parse::<u64>().is_ok())
            && fields
                .next()
                .and_then(|count| count.parse::<u64>().ok())
                .is_some_and(|count| count >= 65536)
    })
}

fn effective_uid_is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|euid| euid == "0"))
        })
        .unwrap_or(false)
}

pub async fn check(paths: &Paths) -> Capabilities {
    let short = Duration::from_secs(10);
    let root = effective_uid_is_root();
    let systemd_version = run("systemctl", &["--version"], &[], short)
        .await
        .ok()
        .and_then(|t| parse_systemd_version(&t));
    let cgroup_v2 = Path::new("/sys/fs/cgroup/cgroup.controllers").exists();
    let podman_version = run("podman", &["version", "--format", "json"], &[], short)
        .await
        .ok()
        .and_then(|t| parse_podman_version(&t));
    let quadlet = paths.quadlet.exists() && Path::new(GENERATOR).exists();
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let subordinate_ids = has_range(&read("/etc/subuid"), "containers")
        && has_range(&read("/etc/subgid"), "containers");

    let mut problems = Vec::new();
    if !root {
        problems.push("the workload backend runs as root (rootful Podman)".to_string());
    }
    match systemd_version {
        Some(v) if v >= MIN_SYSTEMD => {}
        other => problems.push(format!(
            "systemd {MIN_SYSTEMD} or newer is required (found {other:?})"
        )),
    }
    if !cgroup_v2 {
        problems.push("cgroup v2 (unified hierarchy) is required".into());
    }
    match &podman_version {
        Some(v) if version_at_least(v, MIN_PODMAN) => {}
        other => problems.push(format!(
            "Podman {}.{} or newer is required (found {other:?})",
            MIN_PODMAN.0, MIN_PODMAN.1
        )),
    }
    if !quadlet {
        problems.push(format!(
            "the Quadlet generator is required ({} and {GENERATOR})",
            paths.quadlet.display()
        ));
    }
    if !subordinate_ids {
        problems.push(
            "subordinate ID ranges for the `containers` user are required in /etc/subuid and /etc/subgid \
             (for example `containers:2147483647:2147483648`)"
                .into(),
        );
    }
    Capabilities {
        root,
        systemd_version,
        cgroup_v2,
        podman_version,
        quadlet,
        subordinate_ids,
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_systemd_version("systemd 259 (259.5-0ubuntu3.4)\n+PAM +AUDIT"),
            Some(259)
        );
        assert_eq!(parse_systemd_version("garbage"), None);
        let podman = r#"{"Client":{"APIVersion":"5.7.0","Version":"5.7.0"}}"#;
        assert_eq!(parse_podman_version(podman).as_deref(), Some("5.7.0"));
        assert!(version_at_least("5.7.0", (5, 0)));
        assert!(version_at_least("6.2.0", (5, 0)));
        assert!(!version_at_least("4.9.3", (5, 0)));
        assert!(!version_at_least("", (5, 0)));
    }

    #[test]
    fn subordinate_ranges_need_the_containers_user_and_65536_ids() {
        assert!(has_range(
            "andrea:100000:65536\ncontainers:2147483647:2147483648\n",
            "containers"
        ));
        assert!(!has_range("andrea:100000:65536\n", "containers"));
        assert!(!has_range("containers:100000:1000\n", "containers"));
    }
}
