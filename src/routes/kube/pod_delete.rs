use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use k8s_openapi::api::core::v1::Pod;
use kube::api::DeleteParams;
use kube::Api;
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;

use crate::app_state::AppState;

#[derive(Debug, Deserialize, Default, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KubeNamespaceQuery {
    #[param(required = false)]
    pub namespace: Option<String>,
}

#[utoipa::path(
    post,
    path = "/kube/pods/{name}/delete",
    params(
        ("name" = String, Path, description = "Pod name"),
        KubeNamespaceQuery,
    ),
    responses(
        (status = 204, description = "Pod deleted"),
        (status = 404, description = "Pod not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Kube Pods",
    summary = "Delete a Kubernetes pod",
    operation_id = "deletePod"
)]
pub async fn delete_pod_handler(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<KubeNamespaceQuery>,
) -> StatusCode {
    let ks = app.kube.as_ref().unwrap();
    let ns = q.namespace.as_deref().unwrap_or(&ks.namespace);
    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);

    match pods.delete(&name, &DeleteParams::default()).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(kube::Error::Api(ae)) if ae.code == 404 => StatusCode::NOT_FOUND,
        Err(e) => {
            tracing::error!("failed to delete pod {name}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
