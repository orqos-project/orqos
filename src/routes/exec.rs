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
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// JSON payloads
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize, ToSchema)]
pub struct ExecRequest {
    #[schema(example = json!(["ls", "-la", "/data"]))]
    pub cmd: Vec<String>,

    #[serde(default)]
    #[schema(example = "1000:1000")]
    pub user: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExecResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

lazy_static! {
    static ref CONTAINER_ID_RE: Regex = Regex::new(r"^[a-zA-Z0-9_.-]{1,64}$").unwrap();
}

fn validate_container_id(id: &str) -> Result<(), &'static str> {
    if CONTAINER_ID_RE.is_match(id) {
        Ok(())
    } else {
        Err("Invalid container ID format")
    }
}

fn validate_command(cmd: &[String]) -> Result<(), &'static str> {
    if cmd.is_empty() {
        return Err("Command cannot be empty");
    }
    // Add any other policy checks you need (black‑list, length, etc.)
    Ok(())
}

#[utoipa::path(
    post,
    path = "/containers/{id}/exec",
    request_body = ExecRequest,
    responses(
        (status = 200, description = "Command executed successfully", body = ExecResponse),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("id" = String, Path, description = "ID or name of the container"),
    ),
    tag = "Containers",
    operation_id = "exec_in_container",
    summary = "Execute a command in a running container",
    description = "Creates a one-time `docker exec` session inside the specified container and returns the captured stdout/stderr output and exit code."
)]
pub async fn exec_once_handler(
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, (StatusCode, String)> {
    validate_container_id(&container).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    validate_command(&req.cmd).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if req.cmd.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Command cannot be empty".to_string(),
        ));
    }

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
        exit_code: inspect.exit_code.unwrap_or(-1),
    }))
}

/// WebSocket Exec Protocol:
/// ------------------------
/// When a client connects to `/containers/:id/exec/ws`, the server streams
/// stdout and stderr output from the attached `docker exec` session as
/// raw binary WebSocket frames. These frames are unstructured and sent
/// as-is from Docker's output.
///
/// When the process terminates, the server sends a **final text message**
/// in the following format:
///
///     __exit_code:<number>
///
/// This sentinel indicates that the stream has ended and provides the exit
/// status of the command. Clients should monitor for this message to detect
/// completion and trigger any teardown or UI updates accordingly.
///
/// Example:
///     Binary frame: b"hello world\\n"
///     Text frame:   "__exit_code:0"
///
/// Note: The default exit code fallback is `-1` if Docker provides no value.

pub async fn exec_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Query(req): Query<ExecRequest>,
) -> impl IntoResponse {
    // Validate container ID
    if let Err(e) = validate_container_id(&container) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    // Validate command vector
    if let Err(e) = validate_command(&req.cmd) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

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
        // Send final exit code as a specially formatted text message.
        // Clients should detect this and treat it as the end of stream.
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
