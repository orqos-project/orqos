use std::sync::Arc;

use axum::{
    extract::{ws::Message, State, WebSocketUpgrade},
    response::IntoResponse,
};

use crate::app_state::AppState;

#[utoipa::path(
    get,
    path = "/events/ws",
    description = "Exposes Docker and Kubernetes events via WS",
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
        loop {
            tokio::select! {
                incoming = socket.recv() => {
                    if matches!(incoming, None | Some(Err(_)) | Some(Ok(Message::Close(_)))) {
                        break;
                    }
                }
                event = rx.recv() => match event {
                    Ok(event) => {
                        if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    })
}
