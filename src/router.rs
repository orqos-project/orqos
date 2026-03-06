use std::sync::Arc;

use axum::routing::post;
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
use crate::routes::shared::events_ws::events_ws;
use crate::routes::shared::metrics::metrics_handler;
use crate::routes::shared::stats_ws::stats_ws;

#[derive(OpenApi)]
#[openapi(
    info(description = "Orqos API"),
    paths(
        crate::routes::docker::containers_list::list_containers_handler,
        crate::routes::docker::container_stop::stop_container_handler,
        crate::routes::docker::container_remove::remove_container_handler,
        crate::routes::docker::container_create::create_container_handler,
        crate::routes::docker::exec::exec_once_handler,
        crate::routes::docker::write_file::write_file_handler,
        crate::routes::docker::read_file::read_file_handler,
        crate::routes::kube::pods_list::list_pods_handler,
        crate::routes::kube::pod_create::create_pod_handler,
        crate::routes::kube::pod_delete::delete_pod_handler,
        crate::routes::kube::exec::kube_exec_handler,
        crate::routes::shared::events_ws::events_ws,
        crate::routes::shared::stats_ws::stats_ws,
    )
)]
struct ApiDoc;

pub(crate) fn build_router(app: Arc<AppState>) -> Router {
    let mut router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/events/ws", get(events_ws))
        .route("/stats/ws", get(stats_ws));

    if app.docker.is_some() {
        router = router
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
            .route(
                "/docker/containers/{id}/read-file",
                post(read_file_handler),
            );
    }

    if app.kube.is_some() {
        router = router
            .route("/kube/pods", get(list_pods_handler))
            .route("/kube/pods", post(create_pod_handler))
            .route("/kube/pods/{name}/delete", post(delete_pod_handler))
            .route("/kube/pods/{name}/exec", post(kube_exec_handler))
            .route("/kube/pods/{name}/exec/ws", get(kube_exec_ws_handler));
    }

    router
        .with_state(app)
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/swagger")
                .url("/api/openapi.json", ApiDoc::openapi()),
        )
}
