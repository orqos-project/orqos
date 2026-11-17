//! Authenticated `/workload` routes (docs/WORKLOAD_API.md).
use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use super::{capabilities::Capabilities, spec::PodSpec, PodStatus, WorkloadError, WorkloadState};
use crate::app_state::AppState;

fn state(app: &AppState) -> Result<&Arc<WorkloadState>, WorkloadError> {
    app.workload
        .as_ref()
        .ok_or_else(|| WorkloadError::Backend("workload backend is not initialized".into()))
}

async fn authenticate(State(app): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    match &app.workload {
        Some(workload) if workload.authorized(request.headers()) => next.run(request).await,
        Some(_) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Workload control authentication required"})),
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "workload backend is not initialized"})),
        )
            .into_response(),
    }
}

pub fn routes(app: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/workload/capabilities", get(capabilities))
        .route("/workload/pods", get(list))
        .route(
            "/workload/pods/{id}",
            get(inspect).put(apply).delete(remove),
        )
        .route("/workload/pods/{id}/start", post(start))
        .route("/workload/pods/{id}/stop", post(stop))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .route_layer(middleware::from_fn_with_state(app, authenticate))
}

#[utoipa::path(get, path = "/workload/capabilities", security(("workload_bearer" = [])),
    responses((status = 200, description = "Host requirements and what was found", body = Capabilities)), tag = "Workload")]
pub async fn capabilities(
    State(app): State<Arc<AppState>>,
) -> Result<Json<Capabilities>, WorkloadError> {
    Ok(Json(state(&app)?.capabilities().await))
}

#[utoipa::path(get, path = "/workload/pods", security(("workload_bearer" = [])),
    responses((status = 200, description = "Every workload pod with its observed state", body = [PodStatus])), tag = "Workload")]
pub async fn list(State(app): State<Arc<AppState>>) -> Result<Json<Vec<PodStatus>>, WorkloadError> {
    Ok(Json(state(&app)?.list().await?))
}

#[utoipa::path(get, path = "/workload/pods/{id}", security(("workload_bearer" = [])),
    params(("id" = String, Path, description = "Pod ID")),
    responses((status = 200, body = PodStatus), (status = 404, description = "Unknown pod")), tag = "Workload")]
pub async fn inspect(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PodStatus>, WorkloadError> {
    Ok(Json(state(&app)?.get(&id).await?))
}

#[utoipa::path(put, path = "/workload/pods/{id}", security(("workload_bearer" = [])),
    params(("id" = String, Path, description = "Pod ID: a DNS label of at most 48 characters")),
    request_body = PodSpec,
    responses(
        (status = 200, description = "Applied: created, updated (replaced in one go) or unchanged, and started", body = PodStatus),
        (status = 409, description = "Units or a Podman pod with these names exist and are not owned by this pod"),
        (status = 422, description = "Invalid spec, or Quadlet rejected the rendered units"),
        (status = 502, description = "systemd or Podman failed; the message names the command")),
    tag = "Workload")]
pub async fn apply(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(spec): Json<PodSpec>,
) -> Result<Json<PodStatus>, WorkloadError> {
    Ok(Json(state(&app)?.apply(&id, spec).await?))
}

#[utoipa::path(post, path = "/workload/pods/{id}/start", security(("workload_bearer" = [])),
    params(("id" = String, Path, description = "Pod ID")), responses((status = 200, body = PodStatus)), tag = "Workload")]
pub async fn start(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PodStatus>, WorkloadError> {
    Ok(Json(state(&app)?.start(&id).await?))
}

#[utoipa::path(post, path = "/workload/pods/{id}/stop", security(("workload_bearer" = [])),
    params(("id" = String, Path, description = "Pod ID")), responses((status = 200, body = PodStatus)), tag = "Workload")]
pub async fn stop(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PodStatus>, WorkloadError> {
    Ok(Json(state(&app)?.stop(&id).await?))
}

#[utoipa::path(delete, path = "/workload/pods/{id}", security(("workload_bearer" = [])),
    params(("id" = String, Path, description = "Pod ID")),
    responses((status = 200, description = "Stopped, units removed, Podman pod gone")), tag = "Workload")]
pub async fn remove(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, WorkloadError> {
    Ok(Json(state(&app)?.remove(&id).await?))
}
