use futures_util::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{runtime::watcher, runtime::WatchStreamExt, Api, Client as KubeClient};
use serde_json::Value;
use std::time::Duration;
use tokio::{spawn, sync::broadcast, task::JoinHandle, time::sleep};

/// Spawns a background task that watches Kubernetes Pod events and broadcasts
/// them through a shared [`broadcast::Sender`].
///
/// Follows the same pattern as the Docker event fan-out: idle-aware, self-healing
/// with exponential backoff.
pub(crate) fn spawn_kube_event_fanout(
    client: KubeClient,
    tx: broadcast::Sender<Value>,
) -> JoinHandle<()> {
    spawn(async move {
        let mut attempt: u32 = 0;

        tracing::debug!("Starting Kube event fan-out task");

        loop {
            if tx.receiver_count() == 0 {
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            let pods: Api<Pod> = Api::all(client.clone());
            let wc = watcher::Config::default();
            let mut stream = watcher(pods, wc).applied_objects().boxed();
            attempt += 1;
            tracing::debug!(target: "kube-event-fanout", attempt, "subscribing to Kube pod events");

            let mut received_any = false;

            while let Some(event) = stream.next().await {
                match event {
                    Ok(pod) => {
                        received_any = true;
                        if let Ok(js) = serde_json::to_value(&pod) {
                            let wrapped = serde_json::json!({
                                "source": "kube",
                                "event": js
                            });
                            if let Err(err) = tx.send(wrapped) {
                                if tx.receiver_count() == 0 {
                                    tracing::debug!(?err, "all receivers gone; dropping kube events");
                                    break;
                                } else {
                                    tracing::warn!(
                                        ?err,
                                        "failed to deliver Kube event to receivers"
                                    );
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, "Kube watcher error—will reconnect");
                        break;
                    }
                }
            }

            if received_any {
                attempt = 0;
            }

            tracing::debug!("Kube event stream closed—attempting to reconnect");
            let backoff = Duration::from_secs(2u64.pow(attempt.min(5)));
            sleep(backoff).await;
        }
    })
}
