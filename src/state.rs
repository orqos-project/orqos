use bollard::Docker;
use tokio::sync::broadcast;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) docker: Docker,
    pub(crate) events_tx: broadcast::Sender<serde_json::Value>,
}
