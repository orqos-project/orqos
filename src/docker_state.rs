use bollard::Docker;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Clone, Copy)]
pub struct CpuSnapshot {
    pub total_usage: u64,
    pub system_usage: u64,
}

pub struct DockerState {
    pub(crate) docker: Docker,
    pub(crate) cpu_snapshots: RwLock<HashMap<String, CpuSnapshot>>,
}
