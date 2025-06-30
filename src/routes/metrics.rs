use axum::{extract::State, response::IntoResponse};
use std::{sync::Arc, time::Duration};

use crate::state::AppState;

pub async fn metrics_handler(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    // Build a plain-text exposition:
    // rezn_cpu_usage_avg10{container="xyz"} 0.12
    // rezn_mem_usage_max10{container="xyz"} 8.0e+08
    let mut out = String::new();
    for entry in app.metric_registry.cpu.iter() {
        let id = entry.key();
        if let Some(avg) = app.metric_registry.cpu_avg(id, Duration::from_secs(10)) {
            out.push_str(&format!(
                "rezn_cpu_usage_avg10{{container=\"{}\"}} {}\n",
                id, avg
            ));
        }
        if let Some(max_mem) = app.metric_registry.mem_max(id, Duration::from_secs(10)) {
            out.push_str(&format!(
                "rezn_mem_usage_max10{{container=\"{}\"}} {}\n",
                id, max_mem
            ));
        }
    }
    ([(axum::http::header::CONTENT_TYPE, "text/plain")], out)
}
