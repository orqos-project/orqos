use std::{io::Cursor, path::Path as StdPath, sync::Arc};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use k8s_openapi::api::core::v1::Pod;
use kube::api::AttachParams;
use kube::Api;
use serde::Deserialize;
use tar::{Builder, Header};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::docker::write_file::WriteFileResponse;
use crate::routes::kube::exec::kube_exec_capture;

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
    if !payload.path.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be absolute (begin with '/')".into(),
        ));
    }

    if payload.path.contains("/../") || payload.path.contains("/./") {
        return Err((
            StatusCode::BAD_REQUEST,
            "path contains invalid sequences".into(),
        ));
    }

    let ks = state.kube.as_ref().unwrap();
    let ns = payload.namespace.as_deref().unwrap_or(&ks.namespace);

    // Overwrite check
    if payload.overwrite == Some(false) {
        let (_stdout, _stderr, exit_code) = kube_exec_capture(
            &ks.client,
            ns,
            &name,
            payload.container.as_deref(),
            vec!["test".into(), "-e".into(), payload.path.clone()],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

        if exit_code == 0 {
            return Err((
                StatusCode::CONFLICT,
                format!("Refusing to overwrite existing file at {}", payload.path),
            ));
        }
    }

    // Build in-memory tar with just the filename as entry
    let filename = StdPath::new(&payload.path)
        .file_name()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid path".into()))?
        .to_str()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "non-UTF-8 filename".into()))?;

    let mut tar_bytes = Vec::<u8>::new();
    {
        let mut builder = Builder::new(&mut tar_bytes);
        let mut header = Header::new_gnu();
        header.set_size(payload.content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        builder
            .append_data(
                &mut header,
                filename,
                Cursor::new(payload.content.as_bytes()),
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("tar build: {e}"),
                )
            })?;

        builder.finish().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("tar finish: {e}"),
            )
        })?;
    }

    // Extract tar into the parent directory
    let parent_dir = StdPath::new(&payload.path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_owned());

    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);
    let mut ap = AttachParams::default()
        .stdout(false)
        .stderr(true)
        .stdin(true);
    if let Some(ref container) = payload.container {
        ap = ap.container(container.clone());
    }

    let mut attached = pods
        .exec(
            &name,
            vec!["tar", "xf", "-", "-C", &parent_dir],
            &ap,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("exec tar xf: {e}")))?;

    // Write tar bytes to stdin, then close it to signal EOF
    {
        let mut stdin = attached
            .stdin()
            .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "no stdin available".into()))?;
        stdin
            .write_all(&tar_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stdin write: {e}")))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stdin shutdown: {e}")))?;
    }

    // Read stderr for errors
    let mut stderr_bytes = Vec::new();
    if let Some(mut reader) = attached.stderr() {
        reader
            .read_to_end(&mut stderr_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stderr read: {e}")))?;
    }

    let status = attached.take_status().unwrap().await;
    let exit_code = status
        .and_then(|s| {
            s.status
                .as_ref()
                .and_then(|st| if st == "Success" { Some(0) } else { None })
        })
        .unwrap_or(-1);

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
