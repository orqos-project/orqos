pub mod app_state;
pub mod backends;
pub mod docker_state;
pub mod kube_state;
pub mod metric_poller;
pub mod metric_registry;
pub mod router;
pub mod routes;
pub mod spawn_docker_events_fanout;
pub mod spawn_kube_events_fanout;
pub mod stats;

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::app_state::AppState;
use crate::backends::{connect_docker, connect_kube, probe_docker, probe_kube, BackendSelection};
use crate::docker_state::{CpuSnapshot, DockerState};
use crate::kube_state::KubeState;
use crate::metric_poller::poll_docker_metrics;
use crate::metric_registry::MetricRegistry;
use crate::router::build_router;
use crate::spawn_docker_events_fanout::spawn_event_fanout;
use crate::spawn_kube_events_fanout::spawn_kube_event_fanout;
use crate::stats::push_stats_to_ws_clients;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let backends =
        BackendSelection::parse(&env::var("ORQOS_BACKENDS").unwrap_or_else(|_| "both".into()))
            .map_err(anyhow::Error::msg)?;
    let docker_result = if backends.docker {
        connect_docker()
    } else {
        Err("disabled".into())
    };
    let kube_result = if backends.kube {
        connect_kube().await
    } else {
        Err("disabled".into())
    };
    let docker_error = docker_result.as_ref().err().cloned();
    let kube_error = kube_result.as_ref().err().cloned();
    let maybe_docker = docker_result.ok();
    let maybe_kube_client = kube_result.ok();
    let (docker_probe, kube_probe) = tokio::join!(
        async {
            if let Some(docker) = &maybe_docker {
                probe_docker(docker).await
            } else {
                Err(docker_error.clone().unwrap_or_default())
            }
        },
        async {
            if let Some(client) = &maybe_kube_client {
                probe_kube(client).await
            } else {
                Err(kube_error.clone().unwrap_or_default())
            }
        }
    );
    for (name, enabled, result) in [
        ("Docker", backends.docker, &docker_probe),
        ("Kubernetes", backends.kube, &kube_probe),
    ] {
        if enabled {
            match result {
                Ok(()) => info!(backend = name, "Backend is reachable"),
                Err(error) => warn!(backend = name, %error, "Backend is unavailable"),
            }
        }
    }
    if let (Err(docker_error), Err(kube_error)) = (docker_probe, kube_probe) {
        anyhow::bail!(
            "No enabled backend is reachable: Docker: {}; Kubernetes: {}",
            docker_error,
            kube_error
        );
    }

    // Shared broadcast channels
    let (events_tx, _) = broadcast::channel(100);
    let (stats_tx, _) = broadcast::channel(100);

    // Build optional backend states
    let docker_state = maybe_docker.map(|docker| {
        Arc::new(DockerState {
            docker,
            cpu_snapshots: RwLock::<HashMap<String, CpuSnapshot>>::default(),
        })
    });

    let kube_state = maybe_kube_client.map(|client| {
        let namespace = env::var("KUBE_NAMESPACE").unwrap_or_else(|_| "default".into());
        Arc::new(KubeState { client, namespace })
    });

    // Conditionally spawn event fan-outs
    let maybe_docker_event_handle: Option<JoinHandle<()>> = docker_state
        .as_ref()
        .map(|ds| spawn_event_fanout(ds.docker.clone(), events_tx.clone()));

    let maybe_kube_event_handle: Option<JoinHandle<()>> = kube_state
        .as_ref()
        .map(|ks| spawn_kube_event_fanout(ks.client.clone(), events_tx.clone()));

    let metric_registry = MetricRegistry::default();

    let app_state = Arc::new(AppState {
        backends,
        docker_error,
        kube_error,
        docker: docker_state,
        kube: kube_state,
        events_tx,
        stats_tx,
        metric_registry,
    });

    let router = build_router(app_state.clone());

    // Serve HTTP
    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!("Listening on {}", bind_addr);

    let shutdown_signal = async {
        if let Err(e) = signal::ctrl_c().await {
            warn!(?e, "failed to install Ctrl+C handler");
        }
        info!("shutdown signal received - closing HTTP server");
    };

    let state_clone = app_state.clone();

    // Spawn metric polling task
    let metric_handle: JoinHandle<()> = tokio::spawn(async move {
        let interval = Duration::from_secs(5);
        loop {
            if let Some(ref docker) = state_clone.docker {
                if let Err(e) = tokio::time::timeout(
                    Duration::from_secs(30),
                    poll_docker_metrics(docker, &state_clone.metric_registry),
                )
                .await
                {
                    warn!(?e, "Docker metric polling timed out or failed");
                }
            }

            push_stats_to_ws_clients(&state_clone);

            tokio::time::sleep(interval).await;
        }
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // Clean shutdown
    if let Some(handle) = maybe_docker_event_handle {
        handle.abort();
        if let Err(e) = handle.await {
            warn!(?e, "Docker event fan-out task aborted while shutting down");
        }
    }

    if let Some(handle) = maybe_kube_event_handle {
        handle.abort();
        if let Err(e) = handle.await {
            warn!(?e, "Kube event fan-out task aborted while shutting down");
        }
    }

    metric_handle.abort();
    if let Err(e) = metric_handle.await {
        warn!(?e, "metric polling task aborted while shutting down");
    }

    info!("Orqos terminated cleanly");
    Ok(())
}
