use std::sync::Arc;

use axum::routing::post;
use axum::{routing::get, Router};
use utoipa::OpenApi;

use crate::routes::container_create::create_container_handler;
use crate::routes::container_remove::remove_container_handler;
use crate::routes::container_stop::stop_container_handler;
pub use crate::routes::containers_list::list_containers_handler;
use crate::routes::events_ws::events_ws;
use crate::routes::exec::{exec_once_handler, exec_ws_handler};
use crate::routes::metrics::metrics_handler;
use crate::routes::read_file::read_file_handler;
use crate::routes::write_file::write_file_handler;
use crate::state::AppState;

#[derive(OpenApi)]
#[openapi(
    info(description = "Orqos Api"),
    paths(
        crate::routes::containers_list::list_containers_handler,
        crate::routes::container_stop::stop_container_handler,
        crate::routes::container_remove::remove_container_handler,
        crate::routes::container_create::create_container_handler,
        crate::routes::exec::exec_once_handler,
        crate::routes::write_file::write_file_handler,
        crate::routes::read_file::read_file_handler,
        crate::routes::events_ws::events_ws
    )
)]
struct ApiDoc;

pub(crate) fn build_router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/containers", get(list_containers_handler))
        .route("/containers", post(create_container_handler))
        .route("/containers/{id}/stop", post(stop_container_handler))
        .route("/containers/{id}/remove", post(remove_container_handler))
        .route("/containers/{id}/exec", post(exec_once_handler))
        .route("/containers/{id}/exec/ws", get(exec_ws_handler))
        .route("/containers/{id}/write-file", post(write_file_handler))
        .route("/containers/{id}/read-file", post(read_file_handler))
        .route("/metrics", get(metrics_handler))
        .route("/events/ws", get(events_ws))
        .with_state(app)
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/swagger")
                .url("/api/openapi.json", ApiDoc::openapi()),
        )
}
