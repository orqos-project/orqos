use std::time::Duration;

use bollard::{query_parameters::EventsOptionsBuilder, Docker};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::{spawn, sync::broadcast, task::JoinHandle, time::sleep};

/// Spawns a background task that subscribes to Docker events and fan‑outs
/// them through a [`broadcast::Sender`].
///
/// Optimisations:
/// * **Idle‑aware** – if there are *zero* receivers, the task parks itself and
///   doesn’t even subscribe to Docker events, avoiding needless I/O.
/// * **Self‑healing** – on any stream error the task backs off exponentially
///   and retries.
/// * **Log‑level sanity** – only warns when something *should* have worked.
pub(crate) fn spawn_event_fanout(docker: Docker, tx: broadcast::Sender<Value>) -> JoinHandle<()> {
    spawn(async move {
        let mut attempt: u32 = 0;

        tracing::debug!("Starting Docker event fan-out task");

        loop {
            // If no one is listening, wait and re‑check.
            if tx.receiver_count() == 0 {
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            // At least one receiver → subscribe
            let opts = EventsOptionsBuilder::new().build();
            attempt += 1;
            tracing::debug!(target: "event-fanout", attempt, "subscribing to Docker events");

            let mut stream = docker.events(Some(opts));
            let mut received_any = false;

            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(ev) => {
                        received_any = true;
                        if let Ok(js) = serde_json::to_value(&ev) {
                            // If all receivers lag/dropped, `send` errs.
                            if let Err(err) = tx.send(js) {
                                // If receiver count dropped to 0 mid‑flight, downgrade to debug.
                                if tx.receiver_count() == 0 {
                                    tracing::debug!(?err, "all receivers gone; dropping events");
                                    break; // Exit loop; will park again.
                                } else {
                                    tracing::warn!(
                                        ?err,
                                        "failed to deliver Docker event to receivers"
                                    );
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, "Docker event stream error—will reconnect");
                        break;
                    }
                }
            }

            if received_any {
                attempt = 0; // Stream was healthy → reset back‑off.
            }

            tracing::debug!("Docker event stream closed—attempting to reconnect");
            let backoff = Duration::from_secs(2u64.pow(attempt.min(5)));
            sleep(backoff).await;
        }
    })
}
