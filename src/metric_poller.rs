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
                    let cpu_stats = s.cpu_stats.as_ref();
                    let usage = cpu_stats.and_then(|cs| cs.cpu_usage.as_ref());

                    let total = usage.and_then(|cu| cu.total_usage).unwrap_or(0);
                    let sys = cpu_stats.and_then(|cs| cs.system_cpu_usage).unwrap_or(0);
                    let cores = cpu_stats.and_then(|cs| cs.online_cpus).unwrap_or(1) as f64;

                    let mut snapshots = app_state.cpu_snapshots.write().await;

                    let prev = snapshots.get(&id);

                    if let Some(prev) = prev {
                        let cpu_delta = total.saturating_sub(prev.total_usage);
                        let sys_delta = sys.saturating_sub(prev.system_usage);

                        if sys_delta > 0 && cpu_delta > 0 {
                            let cpu_fraction = (cpu_delta as f64 / sys_delta as f64) * cores;
                            app_state.metric_registry.record_cpu(&id, cpu_fraction);
                        } else {
                            app_state.metric_registry.record_cpu(&id, 0.0);
                        }
                    } else {
                        // no previous snapshot — record zero for now
                        app_state.metric_registry.record_cpu(&id, 0.0);
                    }

                    snapshots.insert(
                        id.clone(),
                        CpuSnapshot {
                            total_usage: total,
                            system_usage: sys,
                        },
                    );

                    let mem = s.memory_stats.as_ref().and_then(|m| m.usage).unwrap_or(0);

                    app_state.metric_registry.record_mem(&id, mem);
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
