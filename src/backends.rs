use std::time::Duration;

use bollard::{Docker, API_DEFAULT_VERSION};
use kube::{Client, Config};

#[derive(Clone, Copy, Debug)]
pub struct BackendSelection {
    pub docker: bool,
    pub kube: bool,
}

impl BackendSelection {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "docker" => Ok(Self {
                docker: true,
                kube: false,
            }),
            "kube" => Ok(Self {
                docker: false,
                kube: true,
            }),
            "both" => Ok(Self {
                docker: true,
                kube: true,
            }),
            _ => Err("ORQOS_BACKENDS must be docker, kube, or both".into()),
        }
    }
}

fn docker_socket(socket: Option<&str>, host: Option<&str>) -> Result<String, String> {
    if let Some(socket) = socket {
        let socket = socket.strip_prefix("unix://").unwrap_or(socket);
        if !socket.starts_with('/') {
            return Err("DOCKER_SOCKET must be an absolute Unix socket path".into());
        }
        return Ok(socket.into());
    }
    if let Some(host) = host {
        let socket = host
            .strip_prefix("unix://")
            .filter(|path| path.starts_with('/'))
            .ok_or("DOCKER_HOST must use unix:///absolute/socket/path")?;
        return Ok(socket.into());
    }
    Ok("/var/run/docker.sock".into())
}

pub(crate) fn connect_docker() -> Result<Docker, String> {
    let socket = std::env::var("DOCKER_SOCKET").ok();
    let host = std::env::var("DOCKER_HOST").ok();
    let path = docker_socket(socket.as_deref(), host.as_deref())?;
    tracing::info!(socket = %path, "Selecting Docker socket");
    Docker::connect_with_unix(&path, 30, API_DEFAULT_VERSION).map_err(|error| error.to_string())
}

pub(crate) async fn connect_kube() -> Result<Client, String> {
    let config = tokio::time::timeout(Duration::from_secs(3), Config::infer())
        .await
        .map_err(|_| "Kubernetes configuration timed out".to_string())?
        .map_err(|error| error.to_string())?;
    Client::try_from(config).map_err(|error| error.to_string())
}

pub(crate) async fn probe_docker(docker: &Docker) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(3), docker.ping())
        .await
        .map_err(|_| "Docker reachability check timed out".to_string())?
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) async fn probe_kube(client: &Client) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(3), client.apiserver_version())
        .await
        .map_err(|_| "Kubernetes reachability check timed out".to_string())?
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_modes_are_explicit() {
        let docker = BackendSelection::parse("docker").unwrap();
        assert!(docker.docker && !docker.kube);
        let kube = BackendSelection::parse("kube").unwrap();
        assert!(!kube.docker && kube.kube);
        let both = BackendSelection::parse("both").unwrap();
        assert!(both.docker && both.kube);
        assert!(BackendSelection::parse("unknown").is_err());
    }

    #[test]
    fn explicit_socket_takes_precedence_and_invalid_hosts_fail() {
        assert_eq!(
            docker_socket(Some("/tmp/chosen.sock"), Some("unix:///wrong.sock")).unwrap(),
            "/tmp/chosen.sock"
        );
        assert_eq!(
            docker_socket(None, Some("unix:///tmp/desktop.sock")).unwrap(),
            "/tmp/desktop.sock"
        );
        assert_eq!(docker_socket(None, None).unwrap(), "/var/run/docker.sock");
        assert!(docker_socket(None, Some("tcp://localhost:2375")).is_err());
        assert!(docker_socket(Some("relative.sock"), None).is_err());
    }
}
