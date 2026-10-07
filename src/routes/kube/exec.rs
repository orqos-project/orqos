use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Json, Path, Query, State,
};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use futures::SinkExt;
use futures_util::StreamExt;
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::Status};
use kube::api::AttachParams;
use kube::{Api, Client as KubeClient};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::io::ReaderStream;
use utoipa::{IntoParams, ToSchema};

use crate::app_state::AppState;
use crate::routes::docker::exec::ExecResponse;
use crate::routes::shared::exec::decode_command;

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

    let mut attached = pods.exec(pod, cmd, &ap).await.map_err(|e| e.to_string())?;

    let (stdout_bytes, stderr_bytes) =
        capture_streams(attached.stdout(), attached.stderr()).await?;
    let status = attached.take_status().unwrap().await;
    let exit_code = command_exit_code(status.as_ref());

    Ok((
        stdout_bytes,
        String::from_utf8_lossy(&stderr_bytes).into_owned(),
        exit_code,
    ))
}

pub(crate) fn command_exit_code(status: Option<&Status>) -> i64 {
    let Some(status) = status else {
        return -1;
    };
    if status.status.as_deref() == Some("Success") {
        return 0;
    }
    status
        .details
        .as_ref()
        .and_then(|details| details.causes.as_ref())
        .and_then(|causes| {
            causes
                .iter()
                .find(|cause| cause.reason.as_deref() == Some("ExitCode"))
        })
        .and_then(|cause| cause.message.as_deref()?.parse::<i64>().ok())
        .filter(|code| *code >= 0)
        .unwrap_or(-1)
}

async fn capture_streams<A: AsyncRead + Unpin, B: AsyncRead + Unpin>(
    stdout: Option<A>,
    stderr: Option<B>,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    async fn read<R: AsyncRead + Unpin>(reader: Option<R>) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some(mut reader) = reader {
            reader.read_to_end(&mut bytes).await?;
        }
        Ok(bytes)
    }
    tokio::try_join!(read(stdout), read(stderr)).map_err(|error| error.to_string())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KubeExecWsQuery {
    /// URL-encoded JSON array of command arguments, e.g. ["echo","hello"].
    pub cmd: String,
    pub namespace: Option<String>,
    pub container: Option<String>,
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
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),
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
    if req.cmd.is_empty() || req.cmd[0].is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Command cannot be empty".into()));
    }

    let ks = app.kube.as_ref().unwrap();
    let ns = req.namespace.as_deref().unwrap_or(&ks.namespace);
    let (stdout_bytes, stderr, exit_code) =
        kube_exec_capture(&ks.client, ns, &name, req.container.as_deref(), req.cmd)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr,
        exit_code,
    }))
}

/// WebSocket exec for Kubernetes pods.
///
/// Same protocol as Docker WebSocket exec:
/// - Output frames: `{"stream": "stdout|stderr", "data": "<output>"}`
/// - Termination: `__exit_code:<number>`
#[utoipa::path(
    get, path = "/kube/pods/{name}/exec/ws",
    params(("name" = String, Path, description = "Pod name"), KubeExecWsQuery),
    responses(
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),(status = 101, description = "JSON stdout/stderr frames, then __exit_code:N"),
        (status = 400, description = "Invalid command encoding")),
    tag = "Kube Pods"
)]
pub async fn kube_exec_ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<KubeExecWsQuery>,
) -> impl IntoResponse {
    let cmd = match decode_command(&query.cmd) {
        Ok(cmd) => cmd,
        Err(error) => return error.into_response(),
    };
    let req = KubeExecRequest {
        cmd,
        namespace: query.namespace,
        container: query.container,
    };

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
    let exit_code = command_exit_code(status.as_ref());

    let _ = socket
        .send(Message::Text(format!("__exit_code:{exit_code}").into()))
        .await;
    let _ = socket.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncWriteExt};

    #[test]
    fn preserves_kubernetes_command_exit_codes() {
        let success: Status = serde_json::from_value(json!({"status":"Success"})).unwrap();
        assert_eq!(command_exit_code(Some(&success)), 0);
        for code in [1, 7, 127, 255] {
            let failure: Status = serde_json::from_value(json!({
                "status":"Failure", "reason":"NonZeroExitCode",
                "details":{"causes":[{"reason":"ExitCode","message":code.to_string()}]}
            }))
            .unwrap();
            assert_eq!(command_exit_code(Some(&failure)), code);
        }
        let unknown: Status = serde_json::from_value(json!({"status":"Failure"})).unwrap();
        assert_eq!(command_exit_code(Some(&unknown)), -1);
        assert_eq!(command_exit_code(None), -1);
    }

    #[tokio::test]
    async fn drains_large_stderr_while_stdout_is_still_open() {
        let (stdout, mut stdout_writer) = duplex(64);
        let (stderr, mut stderr_writer) = duplex(64);
        let producer = tokio::spawn(async move {
            stderr_writer.write_all(&vec![b'e'; 131072]).await.unwrap();
            stdout_writer.write_all(b"done").await.unwrap();
        });
        let (out, err) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            capture_streams(Some(stdout), Some(stderr)),
        )
        .await
        .unwrap()
        .unwrap();
        producer.await.unwrap();
        assert_eq!(out, b"done");
        assert_eq!(err, vec![b'e'; 131072]);
    }
}
