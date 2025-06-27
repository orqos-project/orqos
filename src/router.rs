use std::sync::Arc;

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use bollard::secret::ContainerSummary;
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize, Default)]
struct ContainerQuery {
    status: Option<String>, // status=running,exited
    label: Option<String>,  // label=k=v,label=x=y
    name: Option<String>,   // name=foo,name=bar
    all: Option<bool>,
}

async fn list_containers(
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

    app.docker
        .list_containers(Some(opts))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn events_ws(State(app): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        let mut rx = app.events_tx.subscribe();
        while let Ok(ev) = rx.recv().await {
            // Ignore errors if client closed
            let _ = socket.send(Message::Text(ev.to_string().into())).await;
        }
    })
}

pub(crate) fn build_router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/containers", get(list_containers))
        .route("/events", get(events_ws))
        .with_state(app)
}
