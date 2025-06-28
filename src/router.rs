use std::sync::Arc;

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::State;
use axum::routing::post;
use axum::{response::IntoResponse, routing::get, Router};
use utoipa::OpenApi;

use crate::routes::container_create::create_container;
use crate::routes::container_stop::stop_container_handler;
pub use crate::routes::containers_list::list_containers;
use crate::state::AppState;

#[derive(OpenApi)]
#[openapi(
    info(description = "Orqos Api"),
    paths(
        crate::routes::containers_list::list_containers,
        crate::routes::container_stop::stop_container_handler
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
        .route("/containers", get(list_containers))
        .route("/containers", post(create_container))
        .route("/containers/:id/stop", post(stop_container_handler))
        .route("/events", get(events_ws))
        .with_state(app)
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/swagger")
                .url("/api/openapi.json", ApiDoc::openapi()),
        )
}
