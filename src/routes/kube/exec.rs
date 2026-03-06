use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Json, Path, Query, State,
};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use futures::SinkExt;
use futures_util::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::AttachParams;
use kube::{Api, Client as KubeClient};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use utoipa::{IntoParams, ToSchema};

use crate::app_state::AppState;
use crate::routes::docker::exec::ExecResponse;

/// Execute a command in a pod and capture raw stdout bytes, stderr string, and exit code.
pub(crate) async fn kube_exec_capture(
    client: &KubeClient,
    namespace: &str,
    pod: &str,
    container: Option<&str>,
    cmd: Vec<String>,
) -> Result<(Vec<u8>, String, i64), String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let mut ap = AttachParams::default()
        .stdout(true)
        .stderr(true)
        .stdin(false);
    if let Some(c) = container {
        ap = ap.container(c);
    }

    let mut attached = pods
        .exec(pod, cmd, &ap)
        .await
        .map_err(|e| e.to_string())?;

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    if let Some(mut reader) = attached.stdout() {
        reader
            .read_to_end(&mut stdout_bytes)
            .await
            .map_err(|e| format!("stdout: {e}"))?;
    }
    if let Some(mut reader) = attached.stderr() {
        reader
            .read_to_end(&mut stderr_bytes)
            .await
            .map_err(|e| format!("stderr: {e}"))?;
    }

    let status = attached.take_status().unwrap().await;
    let exit_code = status
        .and_then(|s| {
            s.status
                .as_ref()
                .and_then(|st| if st == "Success" { Some(0) } else { None })
        })
        .unwrap_or(-1);

    Ok((
        stdout_bytes,
        String::from_utf8_lossy(&stderr_bytes).into_owned(),
        exit_code,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct KubeExecRequest {
    #[schema(example = json!(["ls", "-la", "/"]))]
    pub cmd: Vec<String>,
    pub namespace: Option<String>,
    pub container: Option<String>,
}

#[derive(Debug, Deserialize, Default, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KubeExecQuery {
    #[param(required = false)]
    pub namespace: Option<String>,
}

#[utoipa::path(
    post,
    path = "/kube/pods/{name}/exec",
    request_body = KubeExecRequest,
    responses(
        (status = 200, description = "Command executed", body = ExecResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("name" = String, Path, description = "Pod name"),
    ),
    tag = "Kube Pods",
    operation_id = "exec_in_pod",
    summary = "Execute a command in a running pod",
)]
pub async fn kube_exec_handler(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<KubeExecRequest>,
) -> Result<Json<ExecResponse>, (StatusCode, String)> {
    if req.cmd.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Command cannot be empty".into()));
    }

    let ks = app.kube.as_ref().unwrap();
    let ns = req.namespace.as_deref().unwrap_or(&ks.namespace);
    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);

    let mut ap = AttachParams::default()
        .stdout(true)
        .stderr(true)
        .stdin(false);

    if let Some(ref container) = req.container {
        ap = ap.container(container.clone());
    }

    let mut attached = pods
        .exec(&name, req.cmd.clone(), &ap)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    if let Some(mut stdout_reader) = attached.stdout() {
        stdout_reader
            .read_to_end(&mut stdout_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stdout read: {e}")))?;
    }

    if let Some(mut stderr_reader) = attached.stderr() {
        stderr_reader
            .read_to_end(&mut stderr_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stderr read: {e}")))?;
    }

    let status = attached.take_status().unwrap().await;
    let exit_code = status
        .and_then(|s| s.status.as_ref().and_then(|st| {
            if st == "Success" {
                Some(0)
            } else {
                // Try to parse exit code from reason/message
                None
            }
        }))
        .unwrap_or(-1);

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        exit_code,
    }))
}

/// WebSocket exec for Kubernetes pods.
///
/// Same protocol as Docker WebSocket exec:
/// - Output frames: `{"stream": "stdout|stderr", "data": "<output>"}`
/// - Termination: `__exit_code:<number>`
pub async fn kube_exec_ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(req): Query<KubeExecRequest>,
) -> impl IntoResponse {
    if req.cmd.is_empty() {
        return (StatusCode::BAD_REQUEST, "Command cannot be empty".to_string()).into_response();
    }

    let ks = app.kube.as_ref().unwrap();
    let client = ks.client.clone();
    let namespace = req.namespace.as_deref().unwrap_or(&ks.namespace).to_owned();

    ws.on_upgrade(move |socket| stream_kube_exec_over_ws(socket, client, namespace, name, req))
}

async fn stream_kube_exec_over_ws(
    mut socket: WebSocket,
    client: KubeClient,
    namespace: String,
    name: String,
    req: KubeExecRequest,
) {
    let pods: Api<Pod> = Api::namespaced(client, &namespace);

    let mut ap = AttachParams::default()
        .stdout(true)
        .stderr(true)
        .stdin(false);

    if let Some(ref container) = req.container {
        ap = ap.container(container.clone());
    }

    let Ok(mut attached) = pods.exec(&name, req.cmd.clone(), &ap).await else {
        let _ = socket
            .send(Message::Text(
                json!({"error": "cannot exec in pod"}).to_string().into(),
            ))
            .await;
        return;
    };

    let status_handle = attached.take_status().unwrap();

    let mut stdout_stream = attached.stdout().map(ReaderStream::new);
    let mut stderr_stream = attached.stderr().map(ReaderStream::new);

    let mut stdout_done = stdout_stream.is_none();
    let mut stderr_done = stderr_stream.is_none();

    while !stdout_done || !stderr_done {
        tokio::select! {
            chunk = async { stdout_stream.as_mut().unwrap().next().await }, if !stdout_done => {
                match chunk {
                    Some(Ok(bytes)) => {
                        let payload = json!({
                            "stream": "stdout",
                            "data": String::from_utf8_lossy(&bytes)
                        });
                        if socket.send(Message::Text(payload.to_string().into())).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(e)) => {
                        let _ = socket
                            .send(Message::Text(format!("error: {e}").into()))
                            .await;
                        return;
                    }
                    None => stdout_done = true,
                }
            }
            chunk = async { stderr_stream.as_mut().unwrap().next().await }, if !stderr_done => {
                match chunk {
                    Some(Ok(bytes)) => {
                        let payload = json!({
                            "stream": "stderr",
                            "data": String::from_utf8_lossy(&bytes)
                        });
                        if socket.send(Message::Text(payload.to_string().into())).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(e)) => {
                        let _ = socket
                            .send(Message::Text(format!("error: {e}").into()))
                            .await;
                        return;
                    }
                    None => stderr_done = true,
                }
            }
        }
    }

    // Send exit code sentinel
    let status = status_handle.await;
    let exit_code = status
        .and_then(|s| {
            s.status
                .as_ref()
                .and_then(|st| if st == "Success" { Some(0) } else { None })
        })
        .unwrap_or(-1);

    let _ = socket
        .send(Message::Text(format!("__exit_code:{exit_code}").into()))
        .await;
    let _ = socket.close().await;
}
