use std::sync::Arc;

use bollard::query_parameters::{ListContainersOptions, StatsOptions};
use futures_util::StreamExt;

use crate::state::{AppState, CpuSnapshot};

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
                    let Some(cpu_stats) = s.cpu_stats else {
                        tracing::debug!("Missing cpu_stats for container {}", id);
                        continue;
                    };
                    let Some(usage) = cpu_stats.cpu_usage else {
                        tracing::debug!("Missing cpu_usage for container {}", id);
                        continue;
                    };
                    let Some(sys) = cpu_stats.system_cpu_usage else {
                        tracing::debug!("Missing system_cpu_usage for container {}", id);
                        continue;
                    };
                    let total = usage.total_usage.unwrap_or_default();
                    let Some(percpu) = usage.percpu_usage else {
                        tracing::debug!("Missing percpu_usage for container {}", id);
                        continue;
                    };
                    let cores = percpu.len().max(1) as f64;

                    let mut snapshots = app_state.cpu_snapshots.write().await;

                    let prev = snapshots.get(&id);

                    if let Some(prev) = prev {
                        let cpu_delta = total.saturating_sub(prev.total_usage);
                        let sys_delta = sys.saturating_sub(prev.system_usage);

                        if sys_delta > 0 && cpu_delta > 0 {
                            let cpu_fraction = (cpu_delta as f64 / sys_delta as f64) * cores;
                            app_state.metric_registry.record_cpu(&id, cpu_fraction);
                        } else {
                            tracing::debug!("Zero delta for container {}, skipping CPU calc", id);
                        }
                    }

                    snapshots.insert(
                        id.clone(),
                        CpuSnapshot {
                            total_usage: total,
                            system_usage: sys,
                        },
                    );

                    let Some(mem_stats) = s.memory_stats else {
                        tracing::debug!("Missing memory_stats for container {}", id);
                        continue;
                    };
                    let Some(mem_bytes) = mem_stats.usage else {
                        tracing::debug!("Missing memory_stats.usage for container {}", id);
                        continue;
                    };
                    app_state.metric_registry.record_mem(&id, mem_bytes);
                }
                Some(Err(e)) => {
                    tracing::debug!("Failed to fetch stats for container {}: {}", id, e);
                }
                None => {
                    tracing::debug!("No stats returned for container {}", id);
                }
            }
        }
    }
}
