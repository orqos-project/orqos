use bollard::Docker;
use std::collections::HashMap;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

use crate::metric_registry::MetricRegistry;

#[derive(Clone, Copy)]
pub struct CpuSnapshot {
    pub total_usage: u64,
    pub system_usage: u64,
}

pub struct AppState {
    pub(crate) docker: Docker,
    pub(crate) events_tx: broadcast::Sender<serde_json::Value>,
    pub(crate) metric_registry: MetricRegistry,
    pub(crate) cpu_snapshots: RwLock<HashMap<String, CpuSnapshot>>,
}
