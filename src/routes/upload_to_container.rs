use std::{io::Cursor, path::Path, sync::Arc};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use bollard::{body_full, query_parameters::UploadToContainerOptions, Docker};
use serde::{Deserialize, Serialize};
use tar::{Builder, Header};

use crate::{
    routes::exec::{exec_once_handler, ExecRequest},
    state::AppState,
};

/// ─────────────────────────────────────────────────────────────
/// Request/response DTOs
/// ─────────────────────────────────────────────────────────────
#[derive(Debug, Deserialize)]
pub struct WriteFileRequest {
    /// **Absolute** path inside the target container
    pub path: String,
    /// Raw UTF-8 file contents (no base64 needed)
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct WriteFileResponse {
    pub status: &'static str,
}

/// ─────────────────────────────────────────────────────────────
/// Main handler
/// ─────────────────────────────────────────────────────────────
pub async fn write_file_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(container_id): AxumPath<String>,
    Json(payload): Json<WriteFileRequest>,
) -> Result<Json<WriteFileResponse>, (StatusCode, String)> {
    // 0) Validate the path we got.
    if !payload.path.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be absolute (begin with '/')".into(),
        ));
    }

    // 1) Build an in-memory tar that contains exactly one file.
    let mut tar_bytes = Vec::<u8>::new();
    {
        let mut builder = Builder::new(&mut tar_bytes);

        // Header describing the single file
        let mut header = Header::new_gnu();
        header.set_size(payload.content.len() as u64);
        header.set_mode(0o644); // regular file 0644
        header.set_cksum();

        // • paths inside the tar **must NOT be absolute** – strip the leading `/`
        let rel_path = &payload.path[1..];

        builder
            .append_data(
                &mut header,
                rel_path,
                Cursor::new(payload.content.as_bytes()),
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("tar build error: {e}"),
                )
            })?;

        builder.finish().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("tar finish: {e}"),
            )
        })?;
    }

    // 2) Stream that tar straight into the container.
    let parent_dir = Path::new(&payload.path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_owned());

    let docker: &Docker = &state.docker;

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

    // 3) Fix ownership and perms through the already-working exec_once_handler  ✅
    //    (we wrap the extractors by hand so we can call it like a normal function)
    use axum::extract::{Path as AxPath, State as AxState};

    // chmod/chown are root-only ⇒ run as "root"
    for cmd in [
        vec!["chown", "devuser:devuser", &payload.path],
        vec!["chmod", "0644", &payload.path],
    ] {
        let exec_req = ExecRequest {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            user: Some("root".into()),
        };

        // Propagate any (status, msg) pair from exec_once_handler if it fails
        let _ = exec_once_handler(
            AxState(state.clone()),
            AxPath(container_id.clone()),
            Json(exec_req),
        )
        .await
        .map_err(|(sc, msg)| (sc, format!("exec {:?} failed: {msg}", cmd)))?;
    }

    Ok(Json(WriteFileResponse { status: "ok" }))
}
