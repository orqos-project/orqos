use axum::{
    extract::{Json, Path, State},
    http::{self, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::{env, path::PathBuf, sync::Arc};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::kube::exec::kube_exec_capture;
use crate::routes::shared::files::{read_archive, read_path};

fn allowed_base() -> PathBuf {
    env::var_os("ORQOS_READ_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home"))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct KubeReadFileRequest {
    /// Absolute path inside the pod
    pub path: String,
    /// Kubernetes namespace (uses default if omitted)
    pub namespace: Option<String>,
    /// Container name (uses default if omitted)
    pub container: Option<String>,
}

/// Pull a single file out of a pod via `tar cf -`.
///
/// `POST /kube/pods/{name}/read-file`
/// Body: `{ "path": "/absolute/path", "namespace": optional, "container": optional }`
/// Response: `200` *application/octet-stream*
#[utoipa::path(
    post,
    path = "/kube/pods/{name}/read-file",
    request_body = KubeReadFileRequest,
    params(
        ("name" = String, Path, description = "Pod name")
    ),
    responses(
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
        (status = 400, description = "Path must name one regular file"),
        (status = 404, description = "File not found"),
        (status = 403, description = "Forbidden path"),
        (status = 500, description = "Server error", body = String)
    ),
    tag = "Kube Pods",
)]
pub async fn kube_read_file_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<KubeReadFileRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let ks = state.kube.as_ref().unwrap();
    let ns = req.namespace.as_deref().unwrap_or(&ks.namespace);

    let target = read_path(&req.path, &allowed_base())?;

    // Strip leading / to get relative path for tar
    let target_string = target.to_string_lossy();
    let rel_path = &target_string[1..];

    let (tar_bytes, stderr, exit_code) = kube_exec_capture(
        &ks.client,
        ns,
        &name,
        req.container.as_deref(),
        vec![
            "tar".into(),
            "cf".into(),
            "-".into(),
            "-C".into(),
            "/".into(),
            rel_path.into(),
        ],
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if exit_code != 0 {
        return Err((
            StatusCode::NOT_FOUND,
            format!("tar failed (exit {exit_code}): {stderr}"),
        ));
    }

    let content = read_archive(tar_bytes)?;

    let mime = infer::get(&content)
        .map(|t| t.mime_type())
        .unwrap_or("application/octet-stream");

    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    Ok((headers, content))
}
