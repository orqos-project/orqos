//! Exec support for Orqos
//! -----------------------------------------------------------
//! * REST   POST /containers/:id/exec        → buffered stdout/stderr + exit‑code (JSON)
//! * WS     GET  /containers/:id/exec/ws     → live stream of stdout/stderr frames
//!
//! All functions are async + Tokio‑friendly.  No Arc<Docker> is needed –
//! `bollard::Docker` is internally Arc‑backed and `Clone`.
//! -----------------------------------------------------------

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Json, Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
};
use bollard::{
    container::LogOutput,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
    Docker,
};
use futures::SinkExt;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// JSON payloads
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct ExecRequest {
    /// Command vector, e.g. ["ls","-la"]
    pub cmd: Vec<String>,

    /// Optional user – UID, UID:GID, or username
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

// ---------------------------------------------------------------------------
// REST endpoint – run once, return collected output
// ---------------------------------------------------------------------------
pub async fn exec_once_handler(
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, (StatusCode, String)> {
    // 1. Create the exec instance
    let exec = state
        .docker
        .create_exec(
            &container,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(req.cmd.clone()),
                user: req.user.clone(),
                ..Default::default()
            },
        )
        .await
        .map_err(err_500)?;

    // 2. Start & attach
    let mut output = match state
        .docker
        .start_exec(&exec.id, Option::<StartExecOptions>::None)
        .await
        .map_err(err_500)?
    {
        StartExecResults::Attached { output, .. } => output,
        StartExecResults::Detached => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Exec started in detached mode, cannot capture output".to_string(),
            ));
        }
    };

    // 3. Drain the stream
    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();

    while let Some(frame) = output.next().await {
        match frame.map_err(err_500)? {
            LogOutput::StdOut { message } => stdout.extend_from_slice(&message),
            LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
            _ => {}
        }
    }

    // 4. Inspect for exit code
    let inspect = state.docker.inspect_exec(&exec.id).await.map_err(err_500)?;

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: inspect.exit_code.unwrap_or_default(),
    }))
}

// ---------------------------------------------------------------------------
// WebSocket endpoint – live streaming
// ---------------------------------------------------------------------------
pub async fn exec_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Query(req): Query<ExecRequest>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_exec_over_ws(socket, state.docker.clone(), container, req))
}

async fn stream_exec_over_ws(
    mut socket: WebSocket,
    docker: Docker,
    container: String,
    req: ExecRequest,
) {
    // 1. create_exec
    let Ok(exec) = docker
        .create_exec(
            &container,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(req.cmd.clone()),
                user: req.user.clone(),
                ..Default::default()
            },
        )
        .await
    else {
        let _ = socket
            .send(Message::Text("error: cannot create exec".into()))
            .await;
        return;
    };

    // 2. start_exec (attached)
    let Ok(StartExecResults::Attached { mut output, .. }) = docker
        .start_exec(&exec.id, Option::<StartExecOptions>::None)
        .await
    else {
        let _ = socket
            .send(Message::Text("error: cannot start exec".into()))
            .await;
        return;
    };

    // 3. Forward frames
    while let Some(frame) = output.next().await {
        match frame {
            Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                // to client
                if socket.send(Message::Binary(message.clone())).await.is_err() {
                    break; // client closed
                }
            }
            Err(e) => {
                let _ = socket
                    .send(Message::Text(format!("error: {e}").into()))
                    .await;
                break;
            }
            _ => {}
        }
    }

    // 4. Final exit code sentinel
    if let Ok(inspect) = docker.inspect_exec(&exec.id).await {
        let msg = format!("__exit_code:{}", inspect.exit_code.unwrap_or(-1));
        let _ = socket.send(Message::Text(msg.into())).await;
    }

    let _ = socket.close().await;
}

// ---------------------------------------------------------------------------
// Helper – convert any error into a 500 tuple and log it
// ---------------------------------------------------------------------------
fn err_500<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    error!("{err}");
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
