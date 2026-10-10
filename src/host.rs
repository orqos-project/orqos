//! Native host mode: authenticated, constrained operations and registered Unix-socket ingress.
use std::{path::PathBuf, process::Stdio, sync::Arc};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{io::AsyncWriteExt, net::UnixStream, process::Command};
use utoipa::ToSchema;

use crate::app_state::AppState;

type HostError = (StatusCode, Json<Value>);
fn failure(message: impl ToString) -> HostError {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error": message.to_string()})),
    )
}

pub struct HostState {
    token: String,
    helper: PathBuf,
}

impl HostState {
    pub async fn from_env() -> anyhow::Result<Self> {
        let token = std::env::var("ORQOS_HOST_TOKEN").map_err(|_| {
            anyhow::anyhow!("Set ORQOS_HOST_TOKEN to a random secret of at least 32 bytes")
        })?;
        anyhow::ensure!(
            token.len() >= 32,
            "ORQOS_HOST_TOKEN must contain at least 32 bytes"
        );
        let state = Self {
            token,
            helper: PathBuf::from("/usr/lib/orqos/host.py"),
        };
        state
            .call(json!({"operation": "health"}))
            .await
            .map_err(|(_, body)| anyhow::anyhow!(body.0.to_string()))?;
        Ok(state)
    }

    pub async fn call(&self, request: Value) -> Result<Value, HostError> {
        let mut child = Command::new("/usr/bin/python3")
            .arg(&self.helper)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(failure)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| failure("Missing helper stdin"))?;
        stdin
            .write_all(&serde_json::to_vec(&request).map_err(failure)?)
            .await
            .map_err(failure)?;
        drop(stdin);
        let output = child.wait_with_output().await.map_err(failure)?;
        let response: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            failure(format!(
                "Host helper failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        })?;
        if let Some(error) = response.get("error") {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": error})),
            ));
        }
        if !output.status.success() {
            return Err(failure("Host helper exited unsuccessfully"));
        }
        response
            .get("ok")
            .cloned()
            .ok_or_else(|| failure("Invalid helper response"))
    }

    fn authorized(&self, request: &Request) -> bool {
        let Some(value) = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|x| x.to_str().ok())
        else {
            return false;
        };
        let Some(candidate) = value.strip_prefix("Bearer ") else {
            return false;
        };
        // All secrets have a fixed public length. Compare every byte.
        candidate.len() == self.token.len()
            && candidate
                .bytes()
                .zip(self.token.bytes())
                .fold(0u8, |difference, (a, b)| difference | (a ^ b))
                == 0
    }
}

async fn authenticate(State(app): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if app
        .host
        .as_ref()
        .is_some_and(|host| host.authorized(&request))
    {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Host control authentication required"})),
        )
            .into_response()
    }
}

pub fn routes(app: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/host/inventory", get(inventory))
        .route("/host/workspaces", get(list))
        .route("/host/workspaces/{id}", get(inspect))
        .route("/host/workspaces/{id}/actions", post(action))
        .route("/host/workspaces/{id}/terminal/", any(terminal_root))
        .route("/host/workspaces/{id}/terminal/{*path}", any(terminal))
        .route(
            "/host/workspaces/{id}/applications/{name}/proxy/",
            any(application_root),
        )
        .route(
            "/host/workspaces/{id}/applications/{name}/proxy/{*path}",
            any(application),
        )
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(app, authenticate))
}

async fn call(app: &AppState, request: Value) -> Result<Json<Value>, HostError> {
    let host = app
        .host
        .as_ref()
        .ok_or_else(|| failure("Host backend is disabled"))?;
    host.call(request).await.map(Json)
}

#[utoipa::path(get, path = "/host/inventory", responses((status=200, description="Operator inventory slots")), security(("host_bearer"=[])), tag="Host")]
pub async fn inventory(State(app): State<Arc<AppState>>) -> Result<Json<Value>, HostError> {
    call(&app, json!({"operation":"inventory"})).await
}

#[utoipa::path(get, path = "/host/workspaces", responses((status=200, description="Registered native workspaces and observed systemd status")), security(("host_bearer"=[])), tag="Host")]
pub async fn list(State(app): State<Arc<AppState>>) -> Result<Json<Value>, HostError> {
    call(&app, json!({"operation":"list"})).await
}

#[utoipa::path(get, path = "/host/workspaces/{id}", params(("id"=String, Path)), responses((status=200, description="Observed workspace status")), security(("host_bearer"=[])), tag="Host")]
pub async fn inspect(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HostError> {
    call(&app, json!({"operation":"inspect", "id":id})).await
}

/// Only these actions can cross the privileged boundary. Identity, paths,
/// service properties, executables and network settings come from root inventory.
#[derive(Deserialize, ToSchema)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HostAction {
    Configure {
        slot: String,
        #[serde(default)]
        limits: HostLimits,
    },
    Start,
    Stop,
    Remove,
    ListFiles,
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    DeleteFiles {
        confirmation: String,
    },
    PutApplication {
        name: String,
        port: u16,
    },
    RemoveApplication {
        name: String,
    },
}

#[derive(Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HostLimits {
    cpu_percent: Option<u32>,
    memory_mb: Option<u32>,
    tasks: Option<u32>,
}

impl HostAction {
    fn request(self, id: String) -> Value {
        let mut request = match self {
            Self::Configure { slot, limits } => {
                let mut values = json!({});
                for (key, value) in [
                    ("cpu_percent", limits.cpu_percent),
                    ("memory_mb", limits.memory_mb),
                    ("tasks", limits.tasks),
                ] {
                    if let Some(value) = value {
                        values[key] = json!(value);
                    }
                }
                json!({"operation":"configure", "slot":slot, "limits": values})
            }
            Self::Start => json!({"operation":"start"}),
            Self::Stop => json!({"operation":"stop"}),
            Self::Remove => json!({"operation":"remove"}),
            Self::ListFiles => json!({"operation":"list-files"}),
            Self::ReadFile { path } => json!({"operation":"read-file", "path":path}),
            Self::WriteFile { path, content } => {
                json!({"operation":"write-file", "path":path, "content":content})
            }
            Self::DeleteFiles { confirmation } => {
                json!({"operation":"delete-files", "confirmation":confirmation})
            }
            Self::PutApplication { name, port } => {
                json!({"operation":"put-application", "name":name, "port":port})
            }
            Self::RemoveApplication { name } => {
                json!({"operation":"remove-application", "name":name})
            }
        };
        request["id"] = json!(id);
        request
    }
}

#[utoipa::path(post, path = "/host/workspaces/{id}/actions", params(("id"=String, Path)), request_body=HostAction, responses((status=200, description="Action result"),(status=422, description="Invalid configuration, ownership mismatch or operation failed")), security(("host_bearer"=[])), tag="Host")]
pub async fn action(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(action): Json<HostAction>,
) -> Result<Json<Value>, HostError> {
    call(&app, action.request(id)).await
}

async fn terminal_root(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response, HostError> {
    forward(app, id, None, String::new(), request).await
}
async fn terminal(
    State(app): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, HostError> {
    forward(app, id, None, path, request).await
}
async fn application_root(
    State(app): State<Arc<AppState>>,
    Path((id, name)): Path<(String, String)>,
    request: Request,
) -> Result<Response, HostError> {
    forward(app, id, Some(name), String::new(), request).await
}
async fn application(
    State(app): State<Arc<AppState>>,
    Path((id, name, path)): Path<(String, String, String)>,
    request: Request,
) -> Result<Response, HostError> {
    forward(app, id, Some(name), path, request).await
}

async fn forward(
    app: Arc<AppState>,
    id: String,
    application: Option<String>,
    _path: String,
    mut request: Request,
) -> Result<Response, HostError> {
    let host = app
        .host
        .as_ref()
        .ok_or_else(|| failure("Host backend disabled"))?;
    let target = host
        .call(json!({"operation":"connect", "id":id, "application":application}))
        .await?;
    let socket = target["socket"]
        .as_str()
        .ok_or_else(|| failure("Invalid registered socket"))?;
    let stream = UnixStream::connect(socket).await.map_err(failure)?;
    // Workspace-owned socket names are mutable. Authenticate the peer before
    // sending any HTTP bytes, so a symlink cannot reach Docker or host services.
    let peer = stream.peer_cred().map_err(failure)?;
    if Some(u64::from(peer.uid())) != target["uid"].as_u64() {
        return Err(failure("Ingress socket has an unexpected Unix identity"));
    }
    let pid = peer
        .pid()
        .ok_or_else(|| failure("Ingress socket has no process identity"))?;
    let expected = target["cgroup"]
        .as_str()
        .filter(|x| !x.is_empty())
        .ok_or_else(|| failure("Workspace has no owned cgroup"))?;
    let groups = tokio::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .await
        .map_err(failure)?;
    if !groups.lines().any(|line| {
        line.strip_prefix("0::")
            .is_some_and(|path| path == expected || path.starts_with(&format!("{expected}/")))
    }) {
        return Err(failure(
            "Ingress socket process is outside the owned workspace cgroup",
        ));
    }

    // Keep the original encoded path. Axum Path decoding must never rewrite
    // percent escapes or application's query strings.
    let raw = request.uri().path();
    let prefix = if let Some(name) = &application {
        format!("/host/workspaces/{id}/applications/{name}/proxy")
    } else {
        format!("/host/workspaces/{id}/terminal")
    };
    let tail = raw
        .strip_prefix(&prefix)
        .ok_or_else(|| failure("Invalid ingress path"))?;
    let path = if application.is_none() {
        format!("/terminal/{id}{tail}")
    } else {
        tail.to_owned()
    };
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    *request.uri_mut() = format!("{path}{query}").parse::<Uri>().map_err(failure)?;
    request.headers_mut().remove(header::AUTHORIZATION);
    if application.is_some() {
        if let Some(value) = request.headers_mut().remove("x-rawpair-app-authorization") {
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
    } else {
        request.headers_mut().remove("x-rawpair-app-authorization");
    }
    if let Some(value) = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        let filtered = value
            .split(';')
            .map(str::trim)
            .filter(|cookie| {
                let name = cookie.split('=').next().unwrap_or("");
                !matches!(
                    name,
                    "__Host-rawpair_preview" | "__Host-rawpair_session" | "_rawpair_key"
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        request.headers_mut().remove(header::COOKIE);
        if !filtered.is_empty() {
            request
                .headers_mut()
                .insert(header::COOKIE, filtered.parse().map_err(failure)?);
        }
    }

    request.headers_mut().remove("x-orqos-token");
    let upgrade = request.headers().get(header::UPGRADE).is_some();
    let downstream = if upgrade {
        Some(hyper::upgrade::on(&mut request))
    } else {
        None
    };
    let (mut sender, connection) = http1::handshake::<_, Body>(TokioIo::new(stream))
        .await
        .map_err(failure)?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            tracing::debug!(%error, "Workspace ingress connection closed");
        }
    });
    let mut response = sender.send_request(request).await.map_err(failure)?;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        if let Some(downstream) = downstream {
            let upstream = hyper::upgrade::on(&mut response);
            tokio::spawn(async move {
                if let (Ok(client), Ok(server)) = tokio::join!(downstream, upstream) {
                    let _ = tokio::io::copy_bidirectional(
                        &mut TokioIo::new(client),
                        &mut TokioIo::new(server),
                    )
                    .await;
                }
            });
        }
    }
    Ok(response.map(Body::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_constrained_actions_deserialize() {
        assert!(serde_json::from_value::<HostAction>(
            json!({"operation":"configure","slot":"a","user":"root"})
        )
        .is_err());
        assert!(
            serde_json::from_value::<HostAction>(json!({"operation":"exec","command":"id"}))
                .is_err()
        );
        let action: HostAction =
            serde_json::from_value(json!({"operation":"configure","slot":"a"})).unwrap();
        assert_eq!(action.request("example".into())["limits"], json!({}));
    }
}
