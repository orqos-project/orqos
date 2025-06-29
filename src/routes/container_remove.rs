use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use bollard::{errors::Error as BollardError, query_parameters::RemoveContainerOptions};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct RemoveContainerRequest {
    pub force: Option<bool>,
    pub v: Option<bool>,
}

#[utoipa::path(
    post,
    path = "/containers/:id/remove",
    params(
        ("id" = String, Path, description = "Container ID or name")
    ),
    request_body(content = RemoveContainerRequest, description = "Stop options", content_type = "application/json"),
    responses(
        (status = 204, description = "Container removed successfully"),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Containers",
)]
pub async fn remove_container_handler(
    State(state): State<Arc<AppState>>,
    Path(container_id): Path<String>,
    maybe_json: Option<Json<RemoveContainerRequest>>,
) -> StatusCode {
    let (force, v) = maybe_json
        .map(|Json(req)| (req.force, req.v))
        .unwrap_or((None, None));

    tracing::debug!("Removing container {container_id} with force: {force:?}, v: {v:?}");

    match remove_container(&state.docker, &container_id, force, v).await {
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

async fn remove_container(
    docker: &bollard::Docker,
    id: &str,
    force: Option<bool>,
    v: Option<bool>,
) -> Result<(), BollardError> {
    docker
        .remove_container(
            id,
            Some(RemoveContainerOptions {
                force: force.unwrap_or(false),
                v: v.unwrap_or(false),
                link: false,
            }),
        )
        .await
}
