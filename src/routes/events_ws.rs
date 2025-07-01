use std::sync::Arc;

use axum::{
    extract::{ws::Message, State, WebSocketUpgrade},
    response::IntoResponse,
};

use crate::state::AppState;

#[utoipa::path(
    get,
    path = "/events/ws",
    description = "Exposes Docker events via WS",
    responses(
        (status = 101, description = "WebSocket upgrade initiated")
    ),
    tag = "Streaming"
)]
pub async fn events_ws(
    State(app): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        let mut rx = app.events_tx.subscribe();
        while let Ok(ev) = rx.recv().await {
            // Ignore errors if client closed
            let _ = socket.send(Message::Text(ev.to_string().into())).await;
        }
    })
}
