use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use k8s_openapi::api::core::v1::Pod;
use kube::api::AttachParams;
use kube::Api;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use utoipa::{IntoParams, ToSchema};

use crate::app_state::AppState;
use crate::routes::docker::exec::ExecResponse;

#[derive(Debug, Deserialize, ToSchema)]
pub struct KubeExecRequest {
    #[schema(example = json!(["ls", "-la", "/"]))]
    pub cmd: Vec<String>,
    pub namespace: Option<String>,
    pub container: Option<String>,
}

#[derive(Debug, Deserialize, Default, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KubeExecQuery {
    #[param(required = false)]
    pub namespace: Option<String>,
}

#[utoipa::path(
    post,
    path = "/kube/pods/{name}/exec",
    request_body = KubeExecRequest,
    responses(
        (status = 200, description = "Command executed", body = ExecResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
    ),
    params(
        ("name" = String, Path, description = "Pod name"),
    ),
    tag = "Kube Pods",
    operation_id = "exec_in_pod",
    summary = "Execute a command in a running pod",
)]
pub async fn kube_exec_handler(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<KubeExecRequest>,
) -> Result<Json<ExecResponse>, (StatusCode, String)> {
    if req.cmd.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Command cannot be empty".into()));
    }

    let ks = app.kube.as_ref().unwrap();
    let ns = req.namespace.as_deref().unwrap_or(&ks.namespace);
    let pods: Api<Pod> = Api::namespaced(ks.client.clone(), ns);

    let mut ap = AttachParams::default()
        .stdout(true)
        .stderr(true)
        .stdin(false);

    if let Some(ref container) = req.container {
        ap = ap.container(container.clone());
    }

    let mut attached = pods
        .exec(&name, req.cmd.clone(), &ap)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    if let Some(mut stdout_reader) = attached.stdout() {
        stdout_reader
            .read_to_end(&mut stdout_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stdout read: {e}")))?;
    }

    if let Some(mut stderr_reader) = attached.stderr() {
        stderr_reader
            .read_to_end(&mut stderr_bytes)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stderr read: {e}")))?;
    }

    let status = attached.take_status().unwrap().await;
    let exit_code = status
        .and_then(|s| s.status.as_ref().and_then(|st| {
            if st == "Success" {
                Some(0)
            } else {
                // Try to parse exit code from reason/message
                None
            }
        }))
        .unwrap_or(-1);

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        exit_code,
    }))
}
