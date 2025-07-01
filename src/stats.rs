use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

#[derive(Debug, Serialize)]
struct Stats {
    cpu_avg: Option<f64>,
    max_mem: Option<u64>,
}

pub fn push_stats_to_ws_clients(app: Arc<AppState>) {
    let mut container_stats: BTreeMap<String, Stats> = BTreeMap::default();

    for entry in app.metric_registry.cpu.iter() {
        let id = entry.key();
        let cpu_avg = app.metric_registry.cpu_avg(id, Duration::from_secs(10));
        let max_mem = app.metric_registry.mem_max(id, Duration::from_secs(10));

        container_stats.insert(id.clone(), Stats { cpu_avg, max_mem });
    }

    match serde_json::to_value(&container_stats) {
        Ok(serialized) => {
            let _ = app.stats_tx.send(serialized);
        }
        Err(e) => {
            tracing::warn!("Failed to serialize container stats: {}", e);
        }
    }
}
