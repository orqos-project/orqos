use axum::{
    extract::{Json, Path, State},
    http::{self, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bollard::query_parameters::DownloadFromContainerOptions;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use std::path::Path as StdPath;
use std::{
    env,
    io::{Cursor, Read},
    path::{Component, PathBuf},
    sync::Arc,
};
use tar::{Archive, EntryType};
use utoipa::ToSchema;

use crate::state::AppState;

fn allowed_base() -> PathBuf {
    env::var_os("ORQOS_READ_BASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home"))
}

fn clean_path(raw: &str) -> Result<PathBuf, &'static str> {
    let p = StdPath::new(raw);

    if !p.is_absolute() {
        return Err("path must be absolute");
    }

    // Normalise: kick out "." and ".." (string-level)
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::RootDir => out.push("/"),
            Component::Normal(c) => out.push(c),
            Component::CurDir => {} // skip .
            Component::ParentDir => return Err("path traversal not allowed"),
            _ => return Err("weird path component"),
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct ReadFileRequest {
    /// Absolute path inside the container
    pub path: String,
}

/// Pull a single file out of a container.
///
/// `POST /containers/{id}/read-file`  
/// Body: `{ "path": "/absolute/path" }`  
/// Response: `200` *application/octet-stream*
#[utoipa::path(
    post,
    path = "/containers/{id}/read_file",
    request_body = ReadFileRequest,
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    responses(
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
        (status = 404, description = "File not found"),
        (status = 500, description = "Docker or server error", body = String)
    )
)]
pub async fn read_file_handler(
    State(state): State<Arc<AppState>>,
    Path(container): Path<String>,
    Json(req): Json<ReadFileRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let base = allowed_base();
    let target: PathBuf =
        clean_path(&req.path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // prefix check (string compare is fine – both are absolute & normalised)
    if !target.starts_with(&base) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("path outside allowed base directory"),
        ));
    }

    // optional hard ban list
    let ban = ["/etc", "/proc", "/sys", "/dev", "/var/run"];
    for bad in ban {
        if target.starts_with(bad) {
            return Err((
                StatusCode::FORBIDDEN,
                "access to system dirs forbidden".into(),
            ));
        }
    }

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
        ..Default::default()
    };

    // Await the API call
    let mut stream = state.docker.download_from_container(&container, Some(opts));

    // Slurp the tar stream into memory
    let mut tar_bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        tar_bytes.extend_from_slice(
            &chunk.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        );
    }

    // Check gzip magic on tar_bytes directly
    let is_gz = tar_bytes.starts_with(&[0x1F, 0x8B]); // gzip magic
    let cursor = Cursor::new(tar_bytes);
    let reader: Box<dyn Read> = if is_gz {
        Box::new(GzDecoder::new(cursor))
    } else {
        Box::new(cursor)
    };
    let mut archive = Archive::new(reader);

    // Expect exactly one entry inside
    let mut entries = archive
        .entries()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut file = entries
        .next()
        .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if file.header().entry_type() == EntryType::Symlink {
        return Err((StatusCode::FORBIDDEN, "symlinks not allowed".into()));
    }

    if entries.next().is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path appears to be a directory".into(),
        ));
    }

    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
