// src/routes/container_stop.rs
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use bollard::errors::Error as BollardError;
use bollard::query_parameters::StopContainerOptions;
use serde::Deserialize;
use std::sync::Arc;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StopContainerRequest {
    pub t: Option<u64>,
    pub signal: Option<String>,
}

/// Stop a running container.  
/// `container_id` can be the ID or name.
/// Returns 204 on success, 404 if the container isn’t found,
/// 500 for anything Bollard spits back.
pub async fn stop_container_handler(
    State(state): State<Arc<AppState>>,
    Path(container_id): Path<String>,
    maybe_json: Option<Json<StopContainerRequest>>,
) -> StatusCode {
    let (t, signal) = maybe_json
        .map(|Json(req)| (req.t, req.signal))
        .unwrap_or((Some(5), None));

    match stop_container(&state.docker, &container_id, t, signal).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(BollardError::DockerResponseServerError { status_code, .. }) if status_code == 404 => {
            StatusCode::NOT_FOUND
        }
        Err(e) => {
            tracing::error!("failed to stop container {container_id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn stop_container(
    docker: &bollard::Docker,
    id: &str,
    t: Option<u64>,
    signal: Option<String>,
) -> Result<(), BollardError> {
    let t_i32 = t.map(|v| v as i32);
    docker
        .stop_container(id, Some(StopContainerOptions { t: t_i32, signal })) // 5-second grace
        .await
}
