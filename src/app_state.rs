use std::sync::Arc;

use tokio::sync::broadcast;

use crate::backends::BackendSelection;
use crate::docker_state::DockerState;
use crate::kube_state::KubeState;
use crate::metric_registry::MetricRegistry;

pub struct AppState {
    pub backends: BackendSelection,
    pub docker_error: Option<String>,
    pub kube_error: Option<String>,
    pub docker: Option<Arc<DockerState>>,
    pub kube: Option<Arc<KubeState>>,
    pub events_tx: broadcast::Sender<serde_json::Value>,
    pub stats_tx: broadcast::Sender<serde_json::Value>,
    pub metric_registry: MetricRegistry,
}
