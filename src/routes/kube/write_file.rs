use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use k8s_openapi::api::core::v1::Pod;
use kube::api::AttachParams;
use kube::Api;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::docker::write_file::WriteFileResponse;
use crate::routes::kube::exec::{command_exit_code, kube_exec_capture};
use crate::routes::shared::files::{check_overwrite, write_archive};

#[derive(Debug, Deserialize, ToSchema)]
pub struct KubeWriteFileRequest {
    /// Absolute path inside the pod
    pub path: String,
    /// Raw UTF-8 file contents
    pub content: String,
    /// Kubernetes namespace (uses default if omitted)
    pub namespace: Option<String>,
    /// Container name (uses default if omitted)
    pub container: Option<String>,
    /// Optional owner string, e.g. "user:group"
    pub owner: Option<String>,
    /// Optional mode string, e.g. "0644"
    pub mode: Option<String>,
    /// If true, overwrite existing file at the given path
    pub overwrite: Option<bool>,
}

/// Write a file into a pod via `tar xf -`.
///
/// `POST /kube/pods/{name}/write-file`
/// Body: `{ "path": "/abs/path", "content": "...", ... }`
/// Response: `200 { "status": "ok" }`
#[utoipa::path(
    post,
    path = "/kube/pods/{name}/write-file",
    request_body = KubeWriteFileRequest,
    params(
        ("name" = String, Path, description = "Pod name")
    ),
    responses(
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),
        (status = 200, description = "File written successfully", body = WriteFileResponse),
        (status = 409, description = "File exists and overwrite is false"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal error"),
    ),
    tag = "Kube Pods",
)]
pub async fn kube_write_file_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(payload): Json<KubeWriteFileRequest>,
) -> Result<Json<WriteFileResponse>, (StatusCode, String)> {
    let (parent_dir, tar_bytes) = write_archive(&payload.path, payload.content.as_bytes())?;

    let ks = state.kube.as_ref().unwrap();
    let ns = payload.namespace.as_deref().unwrap_or(&ks.namespace);

    // Overwrite check
    if payload.overwrite == Some(false) {
        let (_stdout, stderr, exit_code) = kube_exec_capture(
            &ks.client,
            ns,
            &name,
            payload.container.as_deref(),
            vec!["test".into(), "-e".into(), payload.path.clone()],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        check_overwrite(exit_code, &stderr)?;
    }

    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);
    let mut ap = AttachParams::default()
        .stdout(false)
        .stderr(true)
        .stdin(true);
    if let Some(ref container) = payload.container {
        ap = ap.container(container.clone());
    }

    let mut attached = pods
        .exec(&name, vec!["tar", "xf", "-", "-C", &parent_dir], &ap)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("exec tar xf: {e}"),
            )
        })?;

    // Write tar bytes to stdin, then close it to signal EOF
    {
        let mut stdin = attached.stdin().ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "no stdin available".into(),
            )
        })?;
        stdin.write_all(&tar_bytes).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stdin write: {e}"),
            )
        })?;
        stdin.shutdown().await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stdin shutdown: {e}"),
            )
        })?;
    }

    // Read stderr for errors
    let mut stderr_bytes = Vec::new();
    if let Some(mut reader) = attached.stderr() {
        reader.read_to_end(&mut stderr_bytes).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stderr read: {e}"),
            )
        })?;
    }

    let status = attached.take_status().unwrap().await;
    let exit_code = command_exit_code(status.as_ref());

    if exit_code != 0 {
        let stderr_str = String::from_utf8_lossy(&stderr_bytes);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("tar xf failed (exit {exit_code}): {stderr_str}"),
        ));
    }

    // Fix ownership
    if let Some(ref owner) = payload.owner {
        let (_stdout, stderr, exit_code) = kube_exec_capture(
            &ks.client,
            ns,
            &name,
            payload.container.as_deref(),
            vec!["chown".into(), owner.clone(), payload.path.clone()],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        if exit_code != 0 {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("chown failed: {stderr}"),
            ));
        }
    }

    // Fix permissions
    if let Some(ref mode) = payload.mode {
        let (_stdout, stderr, exit_code) = kube_exec_capture(
            &ks.client,
            ns,
            &name,
            payload.container.as_deref(),
            vec!["chmod".into(), mode.clone(), payload.path.clone()],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        if exit_code != 0 {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("chmod failed: {stderr}"),
            ));
        }
    }

    Ok(Json(WriteFileResponse { status: "ok" }))
}
