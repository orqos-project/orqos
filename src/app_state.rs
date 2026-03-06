use std::sync::Arc;

use tokio::sync::broadcast;

use crate::docker_state::DockerState;
use crate::kube_state::KubeState;
use crate::metric_registry::MetricRegistry;

pub struct AppState {
    pub docker: Option<Arc<DockerState>>,
    pub kube: Option<Arc<KubeState>>,
    pub events_tx: broadcast::Sender<serde_json::Value>,
    pub stats_tx: broadcast::Sender<serde_json::Value>,
    pub metric_registry: MetricRegistry,
}
