use bollard::Docker;
use futures::StreamExt;
use tokio::sync::broadcast;

pub(crate) async fn spawn_event_fanout(docker: Docker, tx: broadcast::Sender<serde_json::Value>) {
    tokio::spawn(async move {
        use bollard::query_parameters::EventsOptionsBuilder;
        let opts = EventsOptionsBuilder::new().build();

        let mut stream = docker.events(Some(opts));
        while let Some(Ok(ev)) = stream.next().await {
            // Convert bollard::system::EventMessage ➜ JSON
            if let Ok(js) = serde_json::to_value(ev) {
                // Ignore lagging receivers (Err = no active listeners or slow client)
                let _ = tx.send(js);
            }
        }
    });
}
