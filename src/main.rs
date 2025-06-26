pub mod events;
pub mod router;
pub mod state;

use std::sync::Arc;

use anyhow::Result;
use bollard::Docker;
use bollard::API_DEFAULT_VERSION;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use crate::events::spawn_event_fanout;
use crate::router::build_router;
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
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
    spawn_event_fanout(docker.clone(), tx.clone()).await;

    // 4. Serve HTTP
    let app_state = Arc::new(AppState {
        docker,
        events_tx: tx,
    });
    let router = build_router(app_state);

    tracing::info!("Listening on 0.0.0.0:3000");
    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, router).await?;

    Ok(())
}
