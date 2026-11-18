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

use super::{
    capabilities::Capabilities,
    spec::{PodSpec, SecretSpec, VolumeSpec},
    NetworkStatus, PodStatus, SecretStatus, VolumeStatus, WorkloadError, WorkloadState,
};
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
        .route("/workload/capacity", get(capacity))
        .route("/workload/pods", get(list))
        .route(
            "/workload/pods/{id}",
            get(inspect).put(apply).delete(remove),
        )
        .route("/workload/pods/{id}/start", post(start))
        .route("/workload/pods/{id}/stop", post(stop))
        .route("/workload/networks", get(networks))
        .route("/workload/volumes", get(volumes))
        .route(
            "/workload/volumes/{name}",
            axum::routing::put(apply_volume).delete(remove_volume),
        )
        .route("/workload/secrets", get(secrets))
        .route(
            "/workload/secrets/{name}",
            axum::routing::put(apply_secret).delete(remove_secret),
        )
        // A secret's 1 MiB of data is about 1.4 MiB in base64.
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
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

#[utoipa::path(get, path = "/workload/networks", security(("workload_bearer" = [])),
    responses((status = 200, description = "Owned pod networks and the pods that name them", body = [NetworkStatus])),
    tag = "Workload")]
pub async fn networks(
    State(app): State<Arc<AppState>>,
) -> Result<Json<Vec<NetworkStatus>>, WorkloadError> {
    Ok(Json(state(&app)?.networks().await?))
}

#[utoipa::path(get, path = "/workload/volumes", security(("workload_bearer" = [])),
    responses((status = 200, description = "Owned volumes and the pods that mount them", body = [VolumeStatus])),
    tag = "Workload")]
pub async fn volumes(
    State(app): State<Arc<AppState>>,
) -> Result<Json<Vec<VolumeStatus>>, WorkloadError> {
    Ok(Json(state(&app)?.volumes().await?))
}

#[utoipa::path(put, path = "/workload/volumes/{name}", security(("workload_bearer" = [])),
    params(("name" = String, Path, description = "Volume name: a DNS label of at most 48 characters")),
    request_body = VolumeSpec,
    responses(
        (status = 200, description = "Created, or its spec updated", body = VolumeStatus),
        (status = 409, description = "A Podman volume of that name is not owned by Orqos, or the recorded volume vanished"),
        (status = 422, description = "Invalid name or spec")),
    tag = "Workload")]
pub async fn apply_volume(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<VolumeSpec>,
) -> Result<Json<VolumeStatus>, WorkloadError> {
    Ok(Json(state(&app)?.apply_volume(&name, spec).await?))
}

#[utoipa::path(delete, path = "/workload/volumes/{name}", security(("workload_bearer" = [])),
    params(("name" = String, Path, description = "Volume name")),
    responses(
        (status = 200, description = "The volume and its data are deleted"),
        (status = 404, description = "Unknown volume"),
        (status = 409, description = "Mounted by a pod, or not owned by Orqos")),
    tag = "Workload")]
pub async fn remove_volume(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, WorkloadError> {
    Ok(Json(state(&app)?.remove_volume(&name).await?))
}

#[utoipa::path(get, path = "/workload/secrets", security(("workload_bearer" = [])),
    responses((status = 200, description = "Owned secrets: keys, labels and the pods using them; never values", body = [SecretStatus])),
    tag = "Workload")]
pub async fn secrets(
    State(app): State<Arc<AppState>>,
) -> Result<Json<Vec<SecretStatus>>, WorkloadError> {
    Ok(Json(state(&app)?.secrets().await?))
}

#[utoipa::path(put, path = "/workload/secrets/{name}", security(("workload_bearer" = [])),
    params(("name" = String, Path, description = "Secret name: a DNS label of at most 48 characters")),
    request_body = SecretSpec,
    responses(
        (status = 200, description = "Created, or its labels updated", body = SecretStatus),
        (status = 409, description = "The secret exists with different data (secrets are immutable)"),
        (status = 422, description = "Invalid name or spec")),
    tag = "Workload")]
pub async fn apply_secret(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(spec): Json<SecretSpec>,
) -> Result<Json<SecretStatus>, WorkloadError> {
    Ok(Json(state(&app)?.apply_secret(&name, spec).await?))
}

#[utoipa::path(delete, path = "/workload/secrets/{name}", security(("workload_bearer" = [])),
    params(("name" = String, Path, description = "Secret name")),
    responses(
        (status = 200, description = "The secret is deleted"),
        (status = 404, description = "Unknown secret"),
        (status = 409, description = "Used by a pod")),
    tag = "Workload")]
pub async fn remove_secret(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, WorkloadError> {
    Ok(Json(state(&app)?.remove_secret(&name).await?))
}

#[utoipa::path(get, path = "/workload/capacity", security(("workload_bearer" = [])),
    responses((status = 200, description = "The host's CPUs (× 1000) and memory", body = super::capabilities::Capacity)),
    tag = "Workload")]
pub async fn capacity(
    State(app): State<Arc<AppState>>,
) -> Result<Json<super::capabilities::Capacity>, WorkloadError> {
    state(&app)?;
    Ok(Json(super::capabilities::capacity()))
}
