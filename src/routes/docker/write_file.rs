use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use bollard::{body_full, query_parameters::UploadToContainerOptions, Docker};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::docker::exec::{exec_once_handler, ExecRequest};
use crate::routes::shared::files::{check_command, check_overwrite, write_archive};

/// Request/response DTOs
#[derive(Debug, Deserialize, ToSchema)]
pub struct WriteFileRequest {
    /// **Absolute** path inside the target container
    pub path: String,
    /// Raw UTF-8 file contents (no base64 needed)
    pub content: String,
    /// Optional owner string, e.g. "devuser:devuser"
    pub owner: Option<String>,
    /// Optional mode string, e.g. "0644"
    pub mode: Option<String>,
    /// If true, overwrite existing file at the given path
    pub overwrite: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WriteFileResponse {
    pub status: &'static str,
}

#[utoipa::path(
    post,
    path = "/docker/containers/{id}/write-file",
    request_body = WriteFileRequest,
    responses(
        (status = 503, description = "Enabled backend is unavailable", body = crate::routes::shared::health::BackendHealth),
        (status = 200, description = "File written successfully", body = WriteFileResponse),
        (status = 409, description = "File exists and overwrite is false"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal error"),
    ),
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    tag = "Docker Containers"
)]
pub async fn write_file_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(container_id): AxumPath<String>,
    Json(payload): Json<WriteFileRequest>,
) -> Result<Json<WriteFileResponse>, (StatusCode, String)> {
    let (parent_dir, tar_bytes) = write_archive(&payload.path, payload.content.as_bytes())?;

    if payload.overwrite == Some(false) {
        let exists_req = ExecRequest {
            cmd: vec!["test".into(), "-e".into(), payload.path.clone()],
            user: Some("root".into()),
        };

        let Json(exists_result) = exec_once_handler(
            axum::extract::State(state.clone()),
            axum::extract::Path(container_id.clone()),
            Json(exists_req),
        )
        .await?;
        check_overwrite(exists_result.exit_code, &exists_result.stderr)?;
    }

    let docker: &Docker = &state.docker.as_ref().unwrap().docker;

    docker
        .upload_to_container(
            &container_id,
            Some(UploadToContainerOptions {
                path: parent_dir,
                ..Default::default()
            }),
            body_full(tar_bytes.into()),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("docker cp: {e}")))?;

    // 3) Fix ownership and perms through exec_once_handler
    use axum::extract::{Path as AxPath, State as AxState};

    if let Some(owner) = &payload.owner {
        let exec_req = ExecRequest {
            cmd: vec!["chown".into(), owner.clone(), payload.path.clone()],
            user: Some("root".into()),
        };

        let Json(result) = exec_once_handler(
            AxState(state.clone()),
            AxPath(container_id.clone()),
            Json(exec_req),
        )
        .await
        .map_err(|(sc, msg)| (sc, format!("exec chown failed: {msg}")))?;
        check_command("chown", result.exit_code, &result.stderr)?;
    }

    if let Some(mode) = &payload.mode {
        let exec_req = ExecRequest {
            cmd: vec!["chmod".into(), mode.clone(), payload.path.clone()],
            user: Some("root".into()),
        };

        let Json(result) = exec_once_handler(
            AxState(state.clone()),
            AxPath(container_id.clone()),
            Json(exec_req),
        )
        .await
        .map_err(|(sc, msg)| (sc, format!("exec chmod failed: {msg}")))?;
        check_command("chmod", result.exit_code, &result.stderr)?;
    }

    Ok(Json(WriteFileResponse { status: "ok" }))
}
