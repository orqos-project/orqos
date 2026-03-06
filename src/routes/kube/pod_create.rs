use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use k8s_openapi::api::core::v1::Pod;
use kube::api::PostParams;
use kube::Api;
use std::sync::Arc;

use crate::app_state::AppState;

#[utoipa::path(
    post,
    path = "/kube/pods",
    request_body(
        content = Object,
        description = "Pod manifest JSON",
        content_type = "application/json"
    ),
    responses(
        (status = 201, description = "Pod created", body = Object),
        (status = 400, description = "Invalid manifest"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Kube Pods",
    summary = "Create a new Kubernetes pod",
    operation_id = "createPod"
)]
pub async fn create_pod_handler(
    State(app): State<Arc<AppState>>,
    Json(manifest): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    let ks = app.kube.as_ref().unwrap();

    let ns = manifest
        .pointer("/metadata/namespace")
        .and_then(|v| v.as_str())
        .unwrap_or(&ks.namespace);

    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);

    let pod: Pod = serde_json::from_value(manifest)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid pod manifest: {e}")))?;

    let created = pods
        .create(&PostParams::default(), &pod)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let val = serde_json::to_value(&created)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((StatusCode::CREATED, Json(val)))
}
