use axum::{
    extract::{Json, State},
    http::StatusCode,
};
use bollard::{
    models::{ContainerCreateBody, PortBinding},
    query_parameters::CreateContainerOptions,
    service::HostConfig,
    Docker,
};
use rand::{rng, Rng};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use utoipa::ToSchema;

use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ContainerCreate {
    pub name: String,
    pub image: String,
    pub cpu: Option<String>,    // "2", "1.5"
    pub memory: Option<String>, // "1g"
    pub swap: Option<String>,   // "2g"
    pub env: Option<Vec<String>>,
    pub ports: Option<Vec<PortMap>>, // [{container: 8080, host: 0}]
    pub network: Option<String>,     // "rawpair-net" (defaults to "bridge")
    pub volumes: Option<Vec<VolumeMap>>, // [{source:"/host",target:"/data",ro:false}]
    pub labels: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PortMap {
    pub container: u16,
    pub host: Option<u16>,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct VolumeMap {
    pub source: String,
    pub target: String,
    pub ro: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ContainerInfo {
    pub name: String,
    pub id: String,
    pub ports: std::collections::HashMap<String, u16>,
}

fn pick_host_port() -> Result<u16, String> {
    const MAX_ATTEMPTS: u32 = 100;
    let mut attempts = 0;
    let mut rng_instance = rng();

    loop {
        let p: u16 = rng_instance.random_range(20000..=65535);
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Ok(p);
        }
        attempts += 1;
        if attempts >= MAX_ATTEMPTS {
            return Err(format!(
                "Failed to find a free port after {MAX_ATTEMPTS} attempts"
            ));
        }
    }
}

fn parse_cpu(cpu: &str) -> f64 {
    cpu.parse().unwrap_or(1.0)
}

fn parse_bytes(s: &str) -> u64 {
    let s = s.trim().to_lowercase();
    if let Some(num) = s.strip_suffix("g") {
        num.parse::<u64>().unwrap_or(1) * 1024 * 1024 * 1024
    } else if let Some(num) = s.strip_suffix("m") {
        num.parse::<u64>().unwrap_or(1) * 1024 * 1024
    } else if let Some(num) = s.strip_suffix("k") {
        num.parse::<u64>().unwrap_or(1) * 1024
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
}

#[utoipa::path(
    post,
    path = "/containers",
    request_body(
        content = ContainerCreate,
        description = "Payload to create a container",
        content_type = "application/json",
        example = json!({
            "name": "my-app",
            "image": "nginx:1.27",
            "cpu": "1.5",
            "memory": "1g",
            "env": ["RUST_LOG=info"],
            "labels": { "tier": "backend" },
            "ports": [
                { "container": 80, "host": 8080 },
                { "container": 443 }
            ],
            "network": "my-network",
            "volumes": [
                { "source": "/host/data", "target": "/data", "ro": false }
            ]
        })
    ),
    responses(
        (status = 200, description = "Container created", body = ContainerInfo),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Containers",
    summary = "Create and start a new Docker container",
    operation_id = "createContainer"
)]
pub(crate) async fn create_container_handler(
    State(app): State<Arc<AppState>>,
    Json(req): Json<ContainerCreate>,
) -> Result<Json<ContainerInfo>, (StatusCode, String)> {
    let docker: &Docker = &app.docker;
    let cname = req.name.clone();

    // Ports
    let mut exposed: HashMap<String, HashMap<(), ()>> = HashMap::new();
    let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    let mut port_report: HashMap<String, u16> = HashMap::new();

    if let Some(maps) = &req.ports {
        for &PortMap { container, host } in maps {
            let key = format!("{}/tcp", container);
            let host_port = match host {
                Some(hp) => hp,
                None => pick_host_port().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?,
            };
            exposed.insert(key.clone(), HashMap::new());
            bindings.insert(
                key.clone(),
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".into()),
                    host_port: Some(host_port.to_string()),
                }]),
            );
            port_report.insert(key, host_port);
        }
    }

    // Volumes
    let binds: Option<Vec<String>> = req.volumes.as_ref().map(|vs| {
        vs.iter()
            .map(|VolumeMap { source, target, ro }| {
                let ro_flag = if ro.unwrap_or(false) { ":ro" } else { "" };
                format!("{source}:{target}{ro_flag}")
            })
            .collect()
    });

    // Host config
    let host_cfg = HostConfig {
        network_mode: Some(req.network.clone().unwrap_or_else(|| "bridge".into())),
        binds,
        cpu_quota: req.cpu.as_ref().map(|c| (parse_cpu(c) * 100_000.0) as i64),
        memory: req.memory.as_ref().map(|m| parse_bytes(m) as i64),
        memory_swap: req.swap.as_ref().map(|s| parse_bytes(s) as i64),
        port_bindings: if bindings.is_empty() {
            None
        } else {
            Some(bindings)
        },
        ..Default::default()
    };

    // Container config
    let cfg = ContainerCreateBody {
        image: Some(req.image),
        env: req.env,
        labels: req.labels.clone(),
        exposed_ports: if exposed.is_empty() {
            None
        } else {
            Some(exposed)
        },
        host_config: Some(host_cfg),
        ..Default::default()
    };

    let opts = CreateContainerOptions {
        name: Some(cname.clone()),
        ..Default::default()
    };

    let resp = docker
        .create_container(Some(opts), cfg)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    docker
        .start_container(
            &cname,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ContainerInfo {
        name: cname,
        id: resp.id,
        ports: port_report,
    }))
}
