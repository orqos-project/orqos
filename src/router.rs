use std::sync::Arc;

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::State;
use axum::routing::post;
use axum::{response::IntoResponse, routing::get, Router};
use utoipa::OpenApi;

use crate::routes::container_create::create_container_handler;
use crate::routes::container_stop::stop_container_handler;
pub use crate::routes::containers_list::list_containers_handler;
use crate::routes::exec::{exec_once_handler, exec_ws_handler};
use crate::routes::read_file::read_file_handler;
use crate::routes::write_file::write_file_handler;
use crate::state::AppState;

#[derive(OpenApi)]
#[openapi(
    info(description = "Orqos Api"),
    paths(
        crate::routes::containers_list::list_containers_handler,
        crate::routes::container_stop::stop_container_handler,
        crate::routes::container_create::create_container_handler,
        crate::routes::exec::exec_once_handler,
        crate::routes::write_file::write_file_handler,
        crate::routes::read_file::read_file_handler,
    )
)]
struct ApiDoc;

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
        .route("/containers", get(list_containers_handler))
        .route("/containers", post(create_container_handler))
        .route("/containers/{id}/stop", post(stop_container_handler))
        .route("/containers/{id}/exec", post(exec_once_handler))
        .route("/containers/{id}/exec/ws", get(exec_ws_handler))
        .route("/containers/{id}/write-file", post(write_file_handler))
        .route("/containers/{id}/read-file", post(read_file_handler))
        .route("/events", get(events_ws))
        .with_state(app)
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/swagger")
                .url("/api/openapi.json", ApiDoc::openapi()),
        )
}
