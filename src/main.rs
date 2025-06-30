pub mod events;
pub mod metric_poller;
pub mod metric_registry;
pub mod router;
pub mod routes;
pub mod state;

use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bollard::Docker;
use bollard::API_DEFAULT_VERSION;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::events::spawn_event_fanout;
use crate::metric_poller::poll_metrics_into_registry;
use crate::metric_registry::MetricRegistry;
use crate::router::build_router;
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            // Fall back to Desktop socket if DEFAULT_SOCKET or DOCKER_HOST unset
            let sock = format!("{}/.docker/desktop/docker.sock", std::env::var("HOME")?);
            Docker::connect_with_unix(&sock, 120, API_DEFAULT_VERSION)?
        }
    };
    tracing::info!("Connected to Docker {:?}", docker.version().await?.version);

    // 2. Broadcast channel (100-message ring buffer)
    let (tx, _) = broadcast::channel(100);

    // 3. Spawn fan-out
    let event_handle: JoinHandle<()> = spawn_event_fanout(docker.clone(), tx.clone());

    let metric_registry = MetricRegistry::default();

    // 4. Serve HTTP
    let app_state = Arc::new(AppState {
        docker,
        events_tx: tx,
        metric_registry,
    });
    let router = build_router(app_state.clone());

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

    tokio::spawn(async move {
        let interval = Duration::from_secs(5);

        loop {
            poll_metrics_into_registry(state_clone.clone()).await;
            tokio::time::sleep(interval).await;
        }
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // 6. Clean shutdown: stop event stream task
    event_handle.abort();
    if let Err(e) = event_handle.await {
        warn!(?e, "event fan‑out task aborted while shutting down");
    }

    info!("Orqos terminated cleanly");
    Ok(())
}
