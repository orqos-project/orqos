use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use k8s_openapi::api::core::v1::Pod;
use kube::api::ListParams;
use kube::Api;
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;

use crate::app_state::AppState;

#[derive(Debug, Deserialize, Default, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KubePodQuery {
    #[param(required = false)]
    pub namespace: Option<String>,
    #[param(required = false)]
    pub label_selector: Option<String>,
    #[param(required = false)]
    pub field_selector: Option<String>,
}

#[utoipa::path(
    get,
    path = "/kube/pods",
    params(KubePodQuery),
    responses(
        (status = 200, body = Object)
    ),
    tag = "Kube Pods",
)]
pub async fn list_pods_handler(
    State(app): State<Arc<AppState>>,
    Query(q): Query<KubePodQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let ks = app.kube.as_ref().unwrap();
    let ns = q.namespace.as_deref().unwrap_or(&ks.namespace);
    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);

    let mut lp = ListParams::default();
    if let Some(ref labels) = q.label_selector {
        lp = lp.labels(labels);
    }
    if let Some(ref fields) = q.field_selector {
        lp = lp.fields(fields);
    }

    let pod_list = pods
        .list(&lp)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<serde_json::Value> = pod_list
        .items
        .iter()
        .map(|p| serde_json::to_value(p).unwrap_or_default())
        .collect();

    Ok(Json(items))
}
