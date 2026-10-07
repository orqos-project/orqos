use axum::{
    extract::{Json, Path, State},
    http::{self, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bollard::query_parameters::DownloadFromContainerOptions;
use futures_util::StreamExt;
use std::{env, path::PathBuf, sync::Arc};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::shared::files::{read_archive, read_path};

fn allowed_base() -> PathBuf {
    env::var_os("ORQOS_READ_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home"))
}

#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct ReadFileRequest {
    /// Absolute path inside the container
    pub path: String,
}

/// Pull a single file out of a container.
///
/// `POST /docker/containers/{id}/read-file`
/// Body: `{ "path": "/absolute/path" }`
/// Response: `200` *application/octet-stream*
#[utoipa::path(
    post,
    path = "/docker/containers/{id}/read-file",
    request_body = ReadFileRequest,
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
        (status = 400, description = "Path must name one regular file"),
        (status = 404, description = "File not found"),
        (status = 403, description = "Forbidden path or link"),
        (status = 500, description = "Docker or server error", body = String)
    ),
    tag = "Docker Containers",
)]
pub async fn read_file_handler(
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Json(req): Json<ReadFileRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let docker = &state.docker.as_ref().unwrap().docker;
    let target = read_path(&req.path, &allowed_base())?;

    // 1) Ask the daemon for a tar archive containing `req.path`
    let opts = DownloadFromContainerOptions {
        path: target
            .to_str()
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid UTF-8 path in request".into(),
                )
            })?
            .to_string(),
    };

    // Await the API call
    let mut stream = docker.download_from_container(&container, Some(opts));

    // Slurp the tar stream into memory
    let mut tar_bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        tar_bytes.extend_from_slice(&chunk.map_err(|e| match e {
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                message,
            } => (StatusCode::NOT_FOUND, message),
            other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        })?);
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
