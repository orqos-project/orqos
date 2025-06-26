use std::sync::Arc;

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};

use crate::state::AppState;

async fn list_containers(
    State(app): State<Arc<AppState>>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    use bollard::query_parameters::ListContainersOptionsBuilder;

    let opts = ListContainersOptionsBuilder::new().all(true).build();
    match app.docker.list_containers(Some(opts)).await {
        Ok(list) => Ok(Json(list)),
        Err(e) => Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list containers: {e}"),
        )),
    }
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
