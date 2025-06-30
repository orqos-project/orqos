use std::sync::Arc;

use bollard::query_parameters::{ListContainersOptions, StatsOptions};
use futures_util::StreamExt;

use crate::state::AppState;

pub async fn poll_metrics_into_registry(app_state: Arc<AppState>) {
    if let Ok(containers) = app_state
        .docker
        .list_containers(Some(ListContainersOptions {
            all: false,
            limit: None,
            size: false,
            filters: None,
        }))
        .await
    {
        for c in containers {
            let id = c.id.unwrap_or_default();

            let mut stats_stream = app_state.docker.stats(
                &id,
                Some(StatsOptions {
                    stream: false,
                    one_shot: true,
                }),
            );

            match stats_stream.next().await {
                Some(Ok(s)) => {
                    let cpu_stats = s.cpu_stats.expect("cpu_stats missing");
                    let usage = cpu_stats.cpu_usage.expect("cpu_usage missing");
                    let sys = cpu_stats
                        .system_cpu_usage
                        .expect("system_cpu_usage missing");
                    let total = usage.total_usage.unwrap_or_default();
                    let percpu = usage.percpu_usage.expect("percpu_usage missing");
                    let cores = percpu.len().max(1) as f64;
                    let cpu_fraction = total as f64 / sys as f64 / cores;

                    app_state.metric_registry.record_cpu(&id, cpu_fraction);

                    let mem_stats = s.memory_stats.expect("memory_stats missing");
                    let mem_bytes = mem_stats.usage.expect("memory_stats.usage missing");

                    app_state.metric_registry.record_mem(&id, mem_bytes);
                }
                Some(Err(e)) => {
                    eprintln!("Failed to fetch stats for container {}: {}", id, e);
                }
                None => {
                    eprintln!("No stats returned for container {}", id);
                }
            }
        }
    }
}
