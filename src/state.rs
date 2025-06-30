use bollard::Docker;
use tokio::sync::broadcast;

use crate::metric_registry::MetricRegistry;

#[derive(Clone)]
pub struct AppState {
    pub(crate) docker: Docker,
    pub(crate) events_tx: broadcast::Sender<serde_json::Value>,
    pub(crate) metric_registry: MetricRegistry,
}
