use std::sync::Arc;

use axum::{middleware, routing::post};
use axum::{routing::get, Router};
use utoipa::OpenApi;

use crate::app_state::AppState;
use crate::routes::docker::container_create::create_container_handler;
use crate::routes::docker::container_remove::remove_container_handler;
use crate::routes::docker::container_stop::stop_container_handler;
use crate::routes::docker::containers_list::list_containers_handler;
use crate::routes::docker::exec::{exec_once_handler, exec_ws_handler};
use crate::routes::docker::read_file::read_file_handler;
use crate::routes::docker::write_file::write_file_handler;
use crate::routes::kube::exec::{kube_exec_handler, kube_exec_ws_handler};
use crate::routes::kube::pod_create::create_pod_handler;
use crate::routes::kube::pod_delete::delete_pod_handler;
use crate::routes::kube::pods_list::list_pods_handler;
use crate::routes::kube::read_file::kube_read_file_handler;
use crate::routes::kube::write_file::kube_write_file_handler;
use crate::routes::shared::events_ws::events_ws;
use crate::routes::shared::health::{health_handler, require_docker, require_kube};
use crate::routes::shared::metrics::metrics_handler;
use crate::routes::shared::stats_ws::stats_ws;

#[derive(OpenApi)]
#[openapi(
    info(description = "Orqos API"),
    modifiers(&ControlSecurity),
    paths(
        crate::workload::routes::capabilities,
        crate::workload::routes::list,
        crate::workload::routes::inspect,
        crate::workload::routes::apply,
        crate::workload::routes::start,
        crate::workload::routes::stop,
        crate::workload::routes::remove,
        crate::workload::routes::networks,
        crate::workload::routes::volumes,
        crate::workload::routes::apply_volume,
        crate::workload::routes::remove_volume,
        crate::workload::routes::secrets,
        crate::workload::routes::apply_secret,
        crate::workload::routes::remove_secret,
        crate::host::inventory,
        crate::host::list,
        crate::host::inspect,
        crate::host::action,
        crate::routes::docker::containers_list::list_containers_handler,
        crate::routes::docker::container_stop::stop_container_handler,
        crate::routes::docker::container_remove::remove_container_handler,
        crate::routes::docker::container_create::create_container_handler,
        crate::routes::docker::exec::exec_once_handler,
        crate::routes::docker::exec::exec_ws_handler,
        crate::routes::docker::write_file::write_file_handler,
        crate::routes::docker::read_file::read_file_handler,
        crate::routes::kube::pods_list::list_pods_handler,
        crate::routes::kube::pod_create::create_pod_handler,
        crate::routes::kube::pod_delete::delete_pod_handler,
        crate::routes::kube::exec::kube_exec_handler,
        crate::routes::kube::exec::kube_exec_ws_handler,
        crate::routes::kube::read_file::kube_read_file_handler,
        crate::routes::kube::write_file::kube_write_file_handler,
        crate::routes::shared::events_ws::events_ws,
        crate::routes::shared::metrics::metrics_handler,
        crate::routes::shared::health::health_handler,
        crate::routes::shared::stats_ws::stats_ws,
    )
)]
struct ApiDoc;

struct ControlSecurity;
impl utoipa::Modify for ControlSecurity {
    fn modify(&self, doc: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = doc.components.as_mut() {
            for scheme in ["host_bearer", "workload_bearer"] {
                components.add_security_scheme(
                    scheme,
                    utoipa::openapi::security::SecurityScheme::Http(
                        utoipa::openapi::security::Http::new(
                            utoipa::openapi::security::HttpAuthScheme::Bearer,
                        ),
                    ),
                );
            }
        }
    }
}

pub(crate) fn build_router(app: Arc<AppState>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/events/ws", get(events_ws))
        .route("/stats/ws", get(stats_ws));

    if app.backends.host {
        router = router.merge(crate::host::routes(app.clone()));
    }

    if app.backends.workload {
        router = router.merge(crate::workload::routes::routes(app.clone()));
    }

    if app.backends.docker {
        let docker = Router::new()
            .route("/docker/containers", get(list_containers_handler))
            .route("/docker/containers", post(create_container_handler))
            .route("/docker/containers/{id}/stop", post(stop_container_handler))
            .route(
                "/docker/containers/{id}/remove",
                post(remove_container_handler),
            )
            .route("/docker/containers/{id}/exec", post(exec_once_handler))
            .route("/docker/containers/{id}/exec/ws", get(exec_ws_handler))
            .route(
                "/docker/containers/{id}/write-file",
                post(write_file_handler),
            )
            .route("/docker/containers/{id}/read-file", post(read_file_handler))
            .route_layer(middleware::from_fn_with_state(app.clone(), require_docker));
        router = router.merge(docker);
    }

    if app.backends.kube {
        let kube = Router::new()
            .route("/kube/pods", get(list_pods_handler))
            .route("/kube/pods", post(create_pod_handler))
            .route("/kube/pods/{name}/delete", post(delete_pod_handler))
            .route("/kube/pods/{name}/exec", post(kube_exec_handler))
            .route("/kube/pods/{name}/exec/ws", get(kube_exec_ws_handler))
            .route("/kube/pods/{name}/read-file", post(kube_read_file_handler))
            .route(
                "/kube/pods/{name}/write-file",
                post(kube_write_file_handler),
            )
            .route_layer(middleware::from_fn_with_state(app.clone(), require_kube));
        router = router.merge(kube);
    }

    router.with_state(app).merge(
        utoipa_swagger_ui::SwaggerUi::new("/swagger").url("/api/openapi.json", ApiDoc::openapi()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendSelection;
    use crate::metric_registry::MetricRegistry;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app(selection: &str) -> Arc<AppState> {
        Arc::new(AppState {
            host: None,
            workload: None,
            backends: BackendSelection::parse(selection).unwrap(),
            docker_error: Some("unavailable in tests".into()),
            kube_error: Some("unavailable in tests".into()),
            docker: None,
            kube: None,
            events_tx: tokio::sync::broadcast::channel(1).0,
            stats_tx: tokio::sync::broadcast::channel(1).0,
            metric_registry: MetricRegistry::default(),
        })
    }

    async fn mounted(selection: &str, path: &str) -> bool {
        let response = build_router(app(selection))
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        response.status() != StatusCode::NOT_FOUND
    }

    /// orqos#29: every previous ORQOS_BACKENDS value mounts exactly the routes it did before, and workload
    /// routes appear only when selected.
    #[tokio::test]
    async fn routes_are_mounted_exactly_for_the_selected_backends() {
        let cases = [
            ("docker", [true, false, false, false]),
            ("kube", [false, true, false, false]),
            ("both", [true, true, false, false]),
            ("host", [false, false, true, false]),
            ("workload", [false, false, false, true]),
            ("workload,docker", [true, false, false, true]),
            ("both,workload", [true, true, false, true]),
        ];
        let paths = [
            "/docker/containers",
            "/kube/pods",
            "/host/inventory",
            "/workload/pods",
        ];
        for (selection, expected) in cases {
            for (path, want) in paths.iter().zip(expected) {
                assert_eq!(mounted(selection, path).await, want, "{selection} {path}");
            }
            assert!(mounted(selection, "/healthz").await, "{selection} /healthz");
        }
    }

    #[test]
    fn openapi_documents_workload_routes_with_their_own_bearer_scheme() {
        let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert!(doc["paths"]["/workload/pods/{id}"]["put"].is_object());
        assert!(doc["paths"]["/workload/networks"]["get"].is_object());
        assert!(doc["paths"]["/workload/volumes/{name}"]["put"].is_object());
        assert!(doc["paths"]["/workload/secrets/{name}"]["put"].is_object());
        assert!(doc["components"]["securitySchemes"]["workload_bearer"].is_object());
        assert!(doc["components"]["securitySchemes"]["host_bearer"].is_object());
    }
}
