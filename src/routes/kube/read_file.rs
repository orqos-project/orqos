use axum::{
    extract::{Json, Path, State},
    http::{self, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::{
    env,
    io::{Cursor, Read},
    path::{Component, Path as StdPath, PathBuf},
    sync::Arc,
};
use tar::{Archive, EntryType};
use utoipa::ToSchema;

use crate::app_state::AppState;
use crate::routes::kube::exec::kube_exec_capture;

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
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::RootDir => out.push("/"),
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err("path traversal not allowed"),
            _ => return Err("weird path component"),
        }
    }
    Ok(out)
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
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
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

    let base = allowed_base();
    let target =
        clean_path(&req.path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if !target.starts_with(&base) {
        return Err((
            StatusCode::FORBIDDEN,
            "path outside allowed base directory".into(),
        ));
    }

    let ban = ["/etc", "/proc", "/sys", "/dev", "/var/run"];
    for bad in ban {
        if target.starts_with(bad) {
            return Err((
                StatusCode::FORBIDDEN,
                "access to system dirs forbidden".into(),
            ));
        }
    }

    // Strip leading / to get relative path for tar
    let rel_path = &req.path[1..];

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

    // Decode tar archive
    let is_gz = tar_bytes.starts_with(&[0x1F, 0x8B]);
    let cursor = Cursor::new(tar_bytes);
    let reader: Box<dyn Read> = if is_gz {
        Box::new(GzDecoder::new(cursor))
    } else {
        Box::new(cursor)
    };
    let mut archive = Archive::new(reader);

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
