use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use bollard::models::ContainerSummary;
use serde::Deserialize;
use std::sync::Arc;

use crate::state::AppState;

#[derive(Debug, Deserialize, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContainerQuery {
    #[param(required = false)]
    status: Option<String>, // status=running,exited
    #[param(required = false)]
    label: Option<String>, // label=k=v,label=x=y
    #[param(required = false)]
    name: Option<String>, // name=foo,name=bar
    #[param(required = false)]
    all: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/containers",
    params(ContainerQuery),
    responses(
        (status = 200, body = Object)
    )
)]
pub async fn list_containers_handler(
    State(app): State<Arc<AppState>>,
    Query(q): Query<ContainerQuery>,
) -> Result<Json<Vec<ContainerSummary>>, impl IntoResponse> {
    use bollard::query_parameters::ListContainersOptionsBuilder as Lcob;
    use std::collections::HashMap;

    // Build Docker filter map dynamically
    let mut filters: HashMap<&str, Vec<String>> = HashMap::new();

    if let Some(ref v) = q.label {
        let labels: Vec<String> = v.split(',').map(str::to_owned).collect();
        if !labels.is_empty() {
            filters.insert("label", labels);
        }
    }
    if let Some(ref v) = q.status {
        let statuses: Vec<String> = v.split(',').map(str::to_owned).collect();
        if !statuses.is_empty() {
            filters.insert("status", statuses);
        }
    }
    if let Some(ref v) = q.name {
        let names: Vec<String> = v.split(',').map(str::to_owned).collect();
        if !names.is_empty() {
            filters.insert("name", names);
        }
    }

    let opts = Lcob::new()
        .all(q.all.unwrap_or(false))
        .filters(&filters)
        .build();

    tracing::debug!(?opts, "Listing containers with options");

    app.docker
        .list_containers(Some(opts))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
