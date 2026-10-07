use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{
    app_state::AppState,
    backends::{probe_docker, probe_kube},
};

#[derive(Serialize, ToSchema)]
pub struct BackendHealth {
    pub enabled: bool,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BackendHealth {
    fn new(enabled: bool, result: Result<(), String>) -> Self {
        Self {
            enabled,
            available: enabled && result.is_ok(),
            error: if enabled { result.err() } else { None },
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub docker: BackendHealth,
    pub kube: BackendHealth,
}

impl HealthResponse {
    fn new(docker: BackendHealth, kube: BackendHealth) -> Self {
        let healthy = (!docker.enabled || docker.available) && (!kube.enabled || kube.available);
        Self {
            status: if healthy { "ok" } else { "degraded" },
            docker,
            kube,
        }
    }
    fn status_code(&self) -> StatusCode {
        if self.status == "ok" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn docker_health(app: &AppState) -> BackendHealth {
    let result = if let Some(state) = &app.docker {
        probe_docker(&state.docker).await
    } else {
        Err(app
            .docker_error
            .clone()
            .unwrap_or_else(|| "Docker is disabled".into()))
    };
    BackendHealth::new(app.backends.docker, result)
}

async fn kube_health(app: &AppState) -> BackendHealth {
    let result = if let Some(state) = &app.kube {
        probe_kube(&state.client).await
    } else {
        Err(app
            .kube_error
            .clone()
            .unwrap_or_else(|| "Kubernetes is disabled".into()))
    };
    BackendHealth::new(app.backends.kube, result)
}

#[utoipa::path(
    get, path = "/healthz",
    responses((status = 200, description = "Every enabled backend is reachable", body = HealthResponse),
        (status = 503, description = "An enabled backend is unavailable", body = HealthResponse)),
    tag = "Health"
)]
pub async fn health_handler(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let (docker, kube) = tokio::join!(docker_health(&app), kube_health(&app));
    let response = HealthResponse::new(docker, kube);
    (response.status_code(), Json(response))
}

pub(crate) async fn require_docker(
    State(app): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let health = docker_health(&app).await;
    if health.available {
        next.run(request).await
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(health)).into_response()
    }
}

pub(crate) async fn require_kube(
    State(app): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let health = kube_health(&app).await;
    if health.available {
        next.run(request).await
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(health)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_distinguishes_disabled_and_unavailable_backends() {
        let only_docker = HealthResponse::new(
            BackendHealth::new(true, Ok(())),
            BackendHealth::new(false, Err("disabled".into())),
        );
        assert_eq!(only_docker.status_code(), StatusCode::OK);
        assert!(!only_docker.kube.available);
        assert!(only_docker.kube.error.is_none());
        let degraded = HealthResponse::new(
            BackendHealth::new(true, Ok(())),
            BackendHealth::new(true, Err("connection refused".into())),
        );
        assert_eq!(degraded.status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(degraded.kube.error.as_deref(), Some("connection refused"));
    }
}
