pub mod metric_poller;
pub mod metric_registry;
pub mod router;
pub mod routes;
pub mod spawn_docker_events_fanout;
pub mod docker_state;
pub mod stats;

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bollard::Docker;
use bollard::API_DEFAULT_VERSION;
use kube::{Client as KubeClient, Config as KubeConfig};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::metric_poller::poll_metrics_into_registry;
use crate::metric_registry::MetricRegistry;
use crate::router::build_router;
use crate::spawn_docker_events_fanout::spawn_event_fanout;
use crate::docker_state::DockerAppState;
use crate::docker_state::CpuSnapshot;
use crate::stats::push_stats_to_ws_clients;

async fn try_docker() -> Option<Docker> {
    let docker = Docker::connect_with_local_defaults()
        .or_else(|_| {
            // fallback to Desktop or default Unix socket
            let sock = env::var("DOCKER_SOCKET")
                .unwrap_or_else(|_| "/var/run/docker.sock".to_string());
            Docker::connect_with_unix(&sock, 30, API_DEFAULT_VERSION)
        })
        .ok()?;

    Some(docker)
}

async fn try_kube() -> Option<KubeClient> {
    let config = KubeConfig::infer().await.ok()?;
    let client = KubeClient::try_from(config).ok()?;
    Some(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let maybe_docker = try_docker().await;
    let maybe_kube_client = try_kube().await;

    if maybe_docker.is_none() && maybe_kube_client.is_none() {
        anyhow::bail!("Neither Docker nor Kubernetes is available");
    }
    
    if let Some(docker) = &maybe_docker {
        tracing::info!(
            "Connected to Docker {:?}",
            docker.version().await?.version
        );
    }

    // Events broadcast channel (100-message ring buffer)
    let (events_tx, _) = broadcast::channel(100);

    // Stats broadcast channel (100-message ring buffer)
    let (stats_tx, _) = broadcast::channel(100);

    // Maybe spawn fan-out for docker
    let maybe_docker_event_handle: Option<JoinHandle<()>> = match &maybe_docker {
        Some(docker) => Some(spawn_event_fanout(docker.clone(), events_tx.clone())),
        None => None,
    };

    let metric_registry = MetricRegistry::default();

    let docker_app_state = Arc::new(DockerAppState {
        docker: maybe_docker.unwrap(),
        events_tx,
        stats_tx,
        metric_registry,
        cpu_snapshots: RwLock::<HashMap<String, CpuSnapshot>>::default(),
    });

    let router = build_router(docker_app_state.clone());

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

    let state_clone = docker_app_state.clone();

    // Spawn metric polling task
    let metric_handle: JoinHandle<()> = tokio::spawn(async move {
        let interval = Duration::from_secs(5);
        loop {
            if let Err(e) = tokio::time::timeout(
                Duration::from_secs(30),
                poll_metrics_into_registry(state_clone.clone()),
            )
            .await
            {
                warn!(?e, "Metric polling timed out or failed");
            }

            push_stats_to_ws_clients(state_clone.clone());

            tokio::time::sleep(interval).await;
        }
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // Clean shutdown: stop event stream task
    match maybe_docker_event_handle {
        Some(event_handle) => {
            event_handle.abort();

            if let Err(e) = event_handle.await {
                warn!(?e, "event fan-out task aborted while shutting down");
            }
        }
        None => {
            info!("No Docker event fan-out task to stop");
        }
    }

    // Stop metric polling task
    metric_handle.abort();
    if let Err(e) = metric_handle.await {
        warn!(?e, "metric polling task aborted while shutting down");
    }

    info!("Orqos terminated cleanly");
    Ok(())
}
