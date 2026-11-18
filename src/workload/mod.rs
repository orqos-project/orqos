//! Workload backend (orqos#16–#18): pods of containers run by rootful Podman as owned Quadlet/systemd units.
//! See docs/WORKLOAD_API.md.
pub mod capabilities;
pub mod render;
pub mod routes;
pub mod spec;
pub mod store;
pub mod units;

use std::collections::BTreeMap;

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use tokio::sync::Mutex;
use utoipa::ToSchema;

use spec::{valid_id, valid_volume_name, Medium, PodSpec, VolumeSpec};
use store::{Desired, NetworkRecord, Record, Store, VolumeRecord};
use units::{Ownership, Paths};

#[derive(Debug)]
pub enum WorkloadError {
    Invalid(String),
    NotFound(String),
    Conflict(String),
    Backend(String),
}

impl WorkloadError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Backend(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Invalid(m) | Self::NotFound(m) | Self::Conflict(m) | Self::Backend(m) => m,
        }
    }
}

impl std::fmt::Display for WorkloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl IntoResponse for WorkloadError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status(),
            Json(serde_json::json!({"error": self.message()})),
        )
            .into_response()
    }
}

fn storage(e: anyhow::Error) -> WorkloadError {
    WorkloadError::Backend(format!("workload state: {e:#}"))
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct UnitStatus {
    pub unit: String,
    pub active_state: String,
    pub sub_state: String,
    pub result: String,
    /// Automatic restarts by systemd since the unit was last started explicitly.
    pub restarts: u32,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ContainerStatus {
    pub name: String,
    #[serde(flatten)]
    pub unit: UnitStatus,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PodStatus {
    pub id: String,
    pub desired: Desired,
    /// `running`, `starting`, `restarting`, `degraded`, `failed` or `stopped`, derived from the units.
    pub state: String,
    pub pod: UnitStatus,
    pub containers: Vec<ContainerStatus>,
    pub spec: PodSpec,
    pub error: Option<String>,
    pub updated_at: String,
}

fn unit_status(unit: &str, show: &BTreeMap<String, String>) -> UnitStatus {
    let field = |k: &str| show.get(k).cloned().unwrap_or_default();
    UnitStatus {
        unit: unit.to_string(),
        active_state: field("ActiveState"),
        sub_state: field("SubState"),
        result: field("Result"),
        restarts: field("NRestarts").parse().unwrap_or(0),
    }
}

pub fn summarize(pod: &UnitStatus, containers: &[ContainerStatus]) -> &'static str {
    let states: Vec<&str> = containers
        .iter()
        .map(|c| c.unit.active_state.as_str())
        .collect();
    match pod.active_state.as_str() {
        "failed" => "failed",
        "inactive" if pod.result != "success" && !pod.result.is_empty() => "failed",
        "inactive" | "deactivating" => "stopped",
        "activating" => "starting",
        _ if states.iter().all(|s| *s == "active") => "running",
        _ if containers
            .iter()
            .any(|c| c.unit.sub_state == "auto-restart") =>
        {
            "restarting"
        }
        _ if states.contains(&"activating") => "starting",
        _ if states.contains(&"failed") => "failed",
        _ => "degraded",
    }
}

/// An owned network and the pods that name it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct NetworkStatus {
    pub name: String,
    /// The Podman network, `orqos-net-<name>`.
    pub podman_name: String,
    /// Whether the Podman network currently exists (it is recreated on the next apply or start if not).
    pub exists: bool,
    pub pods: Vec<String>,
    pub created_at: String,
}

/// An owned volume, its spec and the pods that mount it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct VolumeStatus {
    pub name: String,
    /// The Podman volume, `orqos-vol-<name>`.
    pub podman_name: String,
    pub exists: bool,
    pub spec: VolumeSpec,
    pub pods: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// IDs of the pods whose records mount an owned volume.
pub fn mounting<'a>(volume: &str, pods: &'a [Record]) -> Vec<&'a str> {
    pods.iter()
        .filter(|p| p.spec.owned_volumes().any(|v| v == volume))
        .map(|p| p.id.as_str())
        .collect()
}

/// Owned networks that no pod record names.
pub fn unreferenced<'a>(networks: &'a [NetworkRecord], pods: &[Record]) -> Vec<&'a NetworkRecord> {
    networks
        .iter()
        .filter(|n| {
            !pods.iter().any(|p| {
                p.spec
                    .network
                    .as_ref()
                    .is_some_and(|net| net.name == n.name)
            })
        })
        .collect()
}

pub struct WorkloadState {
    token: String,
    paths: Paths,
    store: Store,
    networks: Store<NetworkRecord>,
    volumes: Store<VolumeRecord>,
    // Serializes every operation: unit files, systemd state and records change together.
    lock: Mutex<()>,
}

impl WorkloadState {
    pub async fn from_env() -> anyhow::Result<Self> {
        let token = crate::auth::token_from_env("ORQOS_WORKLOAD_TOKEN")?;
        let paths = Paths::default();
        let capabilities = capabilities::check(&paths).await;
        anyhow::ensure!(
            capabilities.problems.is_empty(),
            "Workload backend unavailable: {}",
            capabilities.problems.join("; ")
        );
        let store = Store::open(&paths.state)?;
        let networks = Store::open(&paths.state.join("networks"))?;
        let volumes = Store::open(&paths.state.join("volumes"))?;
        Ok(Self {
            token,
            paths,
            store,
            networks,
            volumes,
            lock: Mutex::new(()),
        })
    }

    pub fn authorized(&self, headers: &axum::http::HeaderMap) -> bool {
        crate::auth::bearer_matches(headers, &self.token)
    }

    pub async fn capabilities(&self) -> capabilities::Capabilities {
        capabilities::check(&self.paths).await
    }

    fn check_id(id: &str) -> Result<(), WorkloadError> {
        if valid_id(id) {
            Ok(())
        } else {
            Err(WorkloadError::Invalid(
                "pod id: a DNS label of at most 48 characters".into(),
            ))
        }
    }

    fn record(&self, id: &str) -> Result<Record, WorkloadError> {
        Self::check_id(id)?;
        self.store
            .load(id)
            .map_err(storage)?
            .ok_or_else(|| WorkloadError::NotFound(format!("no workload pod {id:?}")))
    }

    /// Refuses to act unless every existing unit and the Podman pod carry this record's token.
    async fn verify_owned(&self, record: &Record) -> Result<(), WorkloadError> {
        let dir = self.paths.units.join(&record.token);
        for unit in units::units_of(&record.id, record.spec.containers.iter().map(|c| &c.name)) {
            if let Ownership::Foreign(why) =
                units::unit_ownership(&units::show(&unit).await?, &record.token, &dir)
            {
                return Err(WorkloadError::Conflict(format!(
                    "{unit} is not owned by pod {}: {why}",
                    record.id
                )));
            }
        }
        let labels = units::pod_labels(&render::pod_name(&record.id)).await?;
        if let Ownership::Foreign(why) = units::label_ownership(labels.as_ref(), &record.token) {
            return Err(WorkloadError::Conflict(format!(
                "Podman pod for {} is not owned by Orqos: {why}",
                record.id
            )));
        }
        Ok(())
    }

    /// A new pod must not collide with units or a Podman pod that already exist under its names.
    async fn verify_free(&self, id: &str, spec: &PodSpec) -> Result<(), WorkloadError> {
        for unit in units::units_of(id, spec.containers.iter().map(|c| &c.name)) {
            if units::show(&unit)
                .await?
                .get("LoadState")
                .map(String::as_str)
                != Some("not-found")
            {
                return Err(WorkloadError::Conflict(format!(
                    "{unit} already exists; refusing to adopt it"
                )));
            }
        }
        if units::pod_labels(&render::pod_name(id)).await?.is_some() {
            return Err(WorkloadError::Conflict(format!(
                "a Podman pod named {} already exists; refusing to adopt it",
                render::pod_name(id)
            )));
        }
        Ok(())
    }

    /// Makes the pod's network exist and verifies that Orqos owns it. A network name seen for the first time gets
    /// a record and a token before the Podman network is created; an existing Podman network of that name that
    /// Orqos has no record of is refused, never adopted. A recorded network that vanished is created again.
    async fn ensure_network(&self, spec: &PodSpec) -> Result<(), WorkloadError> {
        let Some(network) = &spec.network else {
            return Ok(());
        };
        let podman = render::network_name(&network.name);
        let labels = units::network_labels(&podman).await?;
        let record = match self.networks.load(&network.name).map_err(storage)? {
            Some(record) => record,
            None => {
                if labels.is_some() {
                    return Err(WorkloadError::Conflict(format!(
                        "a Podman network named {podman} already exists; refusing to adopt it"
                    )));
                }
                let record = NetworkRecord {
                    name: network.name.clone(),
                    token: store::new_token(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                self.networks.save(&record).map_err(storage)?;
                record
            }
        };
        match units::label_ownership(labels.as_ref(), &record.token) {
            Ownership::Owned => Ok(()),
            Ownership::Absent => units::create_network(&record.name, &record.token).await,
            Ownership::Foreign(why) => Err(WorkloadError::Conflict(format!(
                "Podman network {podman} is not owned by Orqos: {why}"
            ))),
        }
    }

    /// Removes owned networks that no pod names any more. Problems are returned, not raised: the pod operation
    /// that triggered the cleanup has already succeeded, and the periodic check retries.
    async fn release_networks(&self) -> Vec<String> {
        let (networks, pods) = match (self.networks.list(), self.store.list()) {
            (Ok(networks), Ok(pods)) => (networks, pods),
            (Err(e), _) | (_, Err(e)) => return vec![format!("workload state: {e:#}")],
        };
        let mut problems = Vec::new();
        for network in unreferenced(&networks, &pods) {
            let podman = render::network_name(&network.name);
            let result = match units::network_labels(&podman).await {
                Ok(labels) => match units::label_ownership(labels.as_ref(), &network.token) {
                    Ownership::Owned => units::remove_network(&network.name).await,
                    Ownership::Absent => Ok(()),
                    Ownership::Foreign(why) => Err(WorkloadError::Conflict(format!(
                        "Podman network {podman} is not owned by Orqos: {why}; keeping its record"
                    ))),
                },
                Err(e) => Err(e),
            };
            match result.and_then(|()| self.networks.remove(&network.name).map_err(storage)) {
                Ok(()) => {}
                Err(e) => problems.push(format!("network {}: {e}", network.name)),
            }
        }
        problems
    }

    /// Every owned volume the spec mounts must be recorded, present and carry its record's token.
    async fn verify_volumes(&self, spec: &PodSpec) -> Result<(), WorkloadError> {
        for name in spec.owned_volumes() {
            let record = self.volumes.load(name).map_err(storage)?.ok_or_else(|| {
                WorkloadError::Invalid(format!(
                    "unknown volume {name:?}: create it with PUT /workload/volumes/{name} first"
                ))
            })?;
            let podman = render::volume_name(name);
            match units::label_ownership(units::volume_labels(&podman).await?.as_ref(), &record.token) {
                Ownership::Owned => {}
                Ownership::Absent => {
                    return Err(WorkloadError::Conflict(format!(
                        "Podman volume {podman} has vanished; delete volume {name:?} and create it again"
                    )))
                }
                Ownership::Foreign(why) => {
                    return Err(WorkloadError::Conflict(format!(
                        "Podman volume {podman} is not owned by Orqos: {why}"
                    )))
                }
            }
        }
        Ok(())
    }

    /// Creates the pod's missing scratch volumes, owned by the pod's token. A scratch volume of that name owned
    /// by anything else is refused.
    async fn prepare_scratch(
        &self,
        id: &str,
        token: &str,
        spec: &PodSpec,
    ) -> Result<(), WorkloadError> {
        let group = spec.supplementary_groups.first().copied();
        for (name, scratch) in spec.scratch() {
            let podman = render::scratch_name(id, name);
            match units::label_ownership(units::volume_labels(&podman).await?.as_ref(), token) {
                Ownership::Owned => continue,
                Ownership::Foreign(why) => {
                    return Err(WorkloadError::Conflict(format!(
                        "Podman volume {podman} is not owned by pod {id}: {why}"
                    )))
                }
                Ownership::Absent => {}
            }
            let gid = group.unwrap_or(0);
            match scratch.medium {
                Medium::Memory => {
                    let mode = if group.is_some() { "2775" } else { "0777" };
                    let options = [
                        "type=tmpfs".to_string(),
                        "device=tmpfs".to_string(),
                        format!("o=size={},mode={mode},uid=0,gid={gid}", scratch.bytes),
                    ];
                    units::create_volume(&podman, token, (render::POD_LABEL, id), &options).await?;
                }
                Medium::Disk => {
                    let options = [format!("o=uid=0,gid={gid}")];
                    units::create_volume(&podman, token, (render::POD_LABEL, id), &options).await?;
                    units::set_volume_root(&podman, group, 0o777).await?;
                }
            }
        }
        Ok(())
    }

    /// Removes the pod's scratch volumes (only those carrying its token), so a replaced pod starts empty.
    async fn remove_scratch(
        &self,
        id: &str,
        token: &str,
        spec: &PodSpec,
    ) -> Result<(), WorkloadError> {
        for (name, _) in spec.scratch() {
            let podman = render::scratch_name(id, name);
            if units::label_ownership(units::volume_labels(&podman).await?.as_ref(), token)
                == Ownership::Owned
            {
                units::remove_volume(&podman).await?;
            }
        }
        Ok(())
    }

    async fn observe(&self, record: &Record) -> Result<PodStatus, WorkloadError> {
        let pod_unit = render::pod_unit(&record.id);
        let pod = unit_status(&pod_unit, &units::show(&pod_unit).await?);
        let mut containers = Vec::new();
        for c in &record.spec.containers {
            let unit = render::container_unit(&record.id, &c.name);
            containers.push(ContainerStatus {
                name: c.name.clone(),
                unit: unit_status(&unit, &units::show(&unit).await?),
            });
        }
        Ok(PodStatus {
            id: record.id.clone(),
            desired: record.desired,
            state: summarize(&pod, &containers).into(),
            pod,
            containers,
            spec: record.spec.clone(),
            error: record.error.clone(),
            updated_at: record.updated_at.clone(),
        })
    }

    fn save(&self, record: &mut Record, error: Option<String>) -> Result<(), WorkloadError> {
        record.error = error;
        record.updated_at = chrono::Utc::now().to_rfc3339();
        self.store.save(record).map_err(storage)
    }

    async fn stop_units(&self, record: &Record) -> Result<(), WorkloadError> {
        units::systemctl("stop", &render::pod_unit(&record.id)).await?;
        units::reset_failed(&units::units_of(
            &record.id,
            record.spec.containers.iter().map(|c| &c.name),
        ))
        .await;
        Ok(())
    }

    /// Creates or updates a pod. An unchanged, running pod is left alone; a changed one is stopped, its units
    /// replaced, and started again (replacement in one go).
    pub async fn apply(&self, id: &str, spec: PodSpec) -> Result<PodStatus, WorkloadError> {
        Self::check_id(id)?;
        spec.validate().map_err(WorkloadError::Invalid)?;
        let _guard = self.lock.lock().await;
        let existing = self.store.load(id).map_err(storage)?;
        let mut record = match existing {
            Some(record) => {
                self.verify_owned(&record).await?;
                if record.spec == spec && record.desired == Desired::Running {
                    let status = self.observe(&record).await?;
                    if status.state == "running" {
                        return Ok(status);
                    }
                }
                if record.spec != spec {
                    self.stop_units(&record).await?;
                    // A replaced pod starts with empty scratch volumes.
                    self.remove_scratch(id, &record.token, &record.spec).await?;
                }
                record
            }
            None => {
                self.verify_free(id, &spec).await?;
                let now = chrono::Utc::now().to_rfc3339();
                Record {
                    id: id.into(),
                    token: store::new_token(),
                    spec: spec.clone(),
                    desired: Desired::Running,
                    error: None,
                    created_at: now.clone(),
                    updated_at: now,
                }
            }
        };
        let changed = record.spec != spec || !self.paths.units.join(&record.token).exists();
        let left_network =
            record.spec.network.as_ref().map(|n| &n.name) != spec.network.as_ref().map(|n| &n.name);
        self.ensure_network(&spec).await?;
        self.verify_volumes(&spec).await?;
        self.prepare_scratch(id, &record.token, &spec).await?;
        record.spec = spec;
        record.desired = Desired::Running;
        if changed {
            let files = render::render(id, &record.token, &record.spec);
            units::install(&self.paths, id, &record.token, &files).await?;
        }
        self.save(&mut record, None)?;
        if left_network {
            for problem in self.release_networks().await {
                tracing::warn!(%problem, "Releasing an unused workload network failed");
            }
        }
        if let Err(error) = units::systemctl("start", &render::pod_unit(id)).await {
            self.save(&mut record, Some(error.to_string()))?;
            return Err(error);
        }
        self.observe(&record).await
    }

    pub async fn start(&self, id: &str) -> Result<PodStatus, WorkloadError> {
        let _guard = self.lock.lock().await;
        let mut record = self.record(id)?;
        self.verify_owned(&record).await?;
        record.desired = Desired::Running;
        let result = async {
            self.ensure_network(&record.spec).await?;
            self.verify_volumes(&record.spec).await?;
            self.prepare_scratch(id, &record.token, &record.spec)
                .await?;
            units::systemctl("start", &render::pod_unit(id)).await
        }
        .await;
        self.save(&mut record, result.as_ref().err().map(ToString::to_string))?;
        result?;
        self.observe(&record).await
    }

    pub async fn stop(&self, id: &str) -> Result<PodStatus, WorkloadError> {
        let _guard = self.lock.lock().await;
        let mut record = self.record(id)?;
        self.verify_owned(&record).await?;
        record.desired = Desired::Stopped;
        let result = self.stop_units(&record).await;
        self.save(&mut record, result.as_ref().err().map(ToString::to_string))?;
        result?;
        self.observe(&record).await
    }

    /// Stops the pod, removes its units and confirms Podman no longer has its pod.
    pub async fn remove(&self, id: &str) -> Result<serde_json::Value, WorkloadError> {
        let _guard = self.lock.lock().await;
        let record = self.record(id)?;
        self.verify_owned(&record).await?;
        self.stop_units(&record).await?;
        units::uninstall(&self.paths, id, &record.token).await?;
        let name = render::pod_name(id);
        if units::pod_labels(&name).await?.is_some() {
            units::run(
                "podman",
                &["pod", "rm", "--force", &name],
                &[],
                std::time::Duration::from_secs(60),
            )
            .await?;
        }
        self.remove_scratch(id, &record.token, &record.spec).await?;
        self.store.remove(id).map_err(storage)?;
        if record.spec.network.is_some() {
            for problem in self.release_networks().await {
                tracing::warn!(%problem, "Releasing an unused workload network failed");
            }
        }
        Ok(serde_json::json!({"id": id, "status": "removed"}))
    }

    pub async fn get(&self, id: &str) -> Result<PodStatus, WorkloadError> {
        let record = self.record(id)?;
        self.observe(&record).await
    }

    pub async fn list(&self) -> Result<Vec<PodStatus>, WorkloadError> {
        let mut pods = Vec::new();
        for record in self.store.list().map_err(storage)? {
            pods.push(self.observe(&record).await?);
        }
        Ok(pods)
    }

    fn volume_status(&self, record: VolumeRecord, pods: &[Record], exists: bool) -> VolumeStatus {
        VolumeStatus {
            podman_name: render::volume_name(&record.name),
            exists,
            pods: mounting(&record.name, pods)
                .into_iter()
                .map(String::from)
                .collect(),
            name: record.name,
            spec: record.spec,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }

    fn check_volume_name(name: &str) -> Result<(), WorkloadError> {
        if valid_volume_name(name) {
            Ok(())
        } else {
            Err(WorkloadError::Invalid(
                "volume name: a DNS label of at most 48 characters".into(),
            ))
        }
    }

    /// Creates an owned volume or updates its spec. A new volume's root gets the group (mode 2775) or stays
    /// `root:root` 0755; later, the root's group is set again only when it differs (never recursively). An
    /// existing Podman volume without a record is refused, and a recorded one that vanished is reported, not
    /// silently recreated empty.
    pub async fn apply_volume(
        &self,
        name: &str,
        spec: VolumeSpec,
    ) -> Result<VolumeStatus, WorkloadError> {
        Self::check_volume_name(name)?;
        spec.validate().map_err(WorkloadError::Invalid)?;
        let _guard = self.lock.lock().await;
        let podman = render::volume_name(name);
        let labels = units::volume_labels(&podman).await?;
        let now = chrono::Utc::now().to_rfc3339();
        let record = match self.volumes.load(name).map_err(storage)? {
            Some(mut record) => {
                match units::label_ownership(labels.as_ref(), &record.token) {
                    Ownership::Owned => {}
                    Ownership::Absent => {
                        return Err(WorkloadError::Conflict(format!(
                            "Podman volume {podman} has vanished; delete volume {name:?} and create it again"
                        )))
                    }
                    Ownership::Foreign(why) => {
                        return Err(WorkloadError::Conflict(format!(
                            "Podman volume {podman} is not owned by Orqos: {why}"
                        )))
                    }
                }
                if units::volume_root_group(&podman).await? != spec.group.unwrap_or(0) {
                    units::set_volume_root(&podman, spec.group, 0o755).await?;
                }
                if record.spec != spec {
                    record.spec = spec;
                    record.updated_at = now;
                    self.volumes.save(&record).map_err(storage)?;
                }
                record
            }
            None => {
                if labels.is_some() {
                    return Err(WorkloadError::Conflict(format!(
                        "a Podman volume named {podman} already exists; refusing to adopt it"
                    )));
                }
                let record = VolumeRecord {
                    name: name.into(),
                    token: store::new_token(),
                    spec,
                    created_at: now.clone(),
                    updated_at: now,
                };
                self.volumes.save(&record).map_err(storage)?;
                let options = [format!("o=uid=0,gid={}", record.spec.group.unwrap_or(0))];
                units::create_volume(
                    &podman,
                    &record.token,
                    (render::VOLUME_LABEL, name),
                    &options,
                )
                .await?;
                units::set_volume_root(&podman, record.spec.group, 0o755).await?;
                record
            }
        };
        let pods = self.store.list().map_err(storage)?;
        Ok(self.volume_status(record, &pods, true))
    }

    pub async fn volumes(&self) -> Result<Vec<VolumeStatus>, WorkloadError> {
        let pods = self.store.list().map_err(storage)?;
        let mut volumes = Vec::new();
        for record in self.volumes.list().map_err(storage)? {
            let exists = units::volume_labels(&render::volume_name(&record.name))
                .await?
                .is_some();
            volumes.push(self.volume_status(record, &pods, exists));
        }
        Ok(volumes)
    }

    /// Deletes an owned volume and its data. Refused while any pod mounts it.
    pub async fn remove_volume(&self, name: &str) -> Result<serde_json::Value, WorkloadError> {
        Self::check_volume_name(name)?;
        let _guard = self.lock.lock().await;
        let record = self
            .volumes
            .load(name)
            .map_err(storage)?
            .ok_or_else(|| WorkloadError::NotFound(format!("no volume {name:?}")))?;
        let pods = self.store.list().map_err(storage)?;
        let users = mounting(name, &pods);
        if !users.is_empty() {
            return Err(WorkloadError::Conflict(format!(
                "volume {name:?} is mounted by pods {users:?}"
            )));
        }
        let podman = render::volume_name(name);
        match units::label_ownership(units::volume_labels(&podman).await?.as_ref(), &record.token) {
            Ownership::Owned => units::remove_volume(&podman).await?,
            Ownership::Absent => {}
            Ownership::Foreign(why) => {
                return Err(WorkloadError::Conflict(format!(
                    "Podman volume {podman} is not owned by Orqos: {why}"
                )))
            }
        }
        self.volumes.remove(name).map_err(storage)?;
        Ok(serde_json::json!({"name": name, "status": "removed"}))
    }

    pub async fn networks(&self) -> Result<Vec<NetworkStatus>, WorkloadError> {
        let pods = self.store.list().map_err(storage)?;
        let mut networks = Vec::new();
        for record in self.networks.list().map_err(storage)? {
            let podman_name = render::network_name(&record.name);
            networks.push(NetworkStatus {
                exists: units::network_labels(&podman_name).await?.is_some(),
                podman_name,
                pods: pods
                    .iter()
                    .filter(|p| {
                        p.spec
                            .network
                            .as_ref()
                            .is_some_and(|n| n.name == record.name)
                    })
                    .map(|p| p.id.clone())
                    .collect(),
                name: record.name,
                created_at: record.created_at,
            });
        }
        Ok(networks)
    }

    /// Periodic check: pods that should run but whose units failed or vanished are reported.
    pub async fn reconcile(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let records = match self.store.list() {
            Ok(records) => records,
            Err(e) => return vec![format!("workload state: {e:#}")],
        };
        for record in records.iter().filter(|r| r.desired == Desired::Running) {
            match self.observe(record).await {
                Ok(status) if status.state == "running" => {}
                Ok(status) => problems.push(format!("{}: {}", record.id, status.state)),
                Err(e) => problems.push(format!("{}: {e}", record.id)),
            }
        }
        // Networks left behind by an interrupted removal.
        let _guard = self.lock.lock().await;
        problems.extend(self.release_networks().await);
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(active: &str, sub: &str, result: &str) -> UnitStatus {
        UnitStatus {
            unit: "u".into(),
            active_state: active.into(),
            sub_state: sub.into(),
            result: result.into(),
            restarts: 0,
        }
    }

    fn containers(states: &[(&str, &str)]) -> Vec<ContainerStatus> {
        states
            .iter()
            .map(|(a, s)| ContainerStatus {
                name: "c".into(),
                unit: unit(a, s, "success"),
            })
            .collect()
    }

    #[test]
    fn summaries_follow_unit_states() {
        let active = unit("active", "running", "success");
        assert_eq!(
            summarize(
                &active,
                &containers(&[("active", "running"), ("active", "running")])
            ),
            "running"
        );
        assert_eq!(
            summarize(
                &active,
                &containers(&[("active", "running"), ("activating", "auto-restart")])
            ),
            "restarting"
        );
        assert_eq!(
            summarize(&active, &containers(&[("activating", "start")])),
            "starting"
        );
        assert_eq!(
            summarize(&active, &containers(&[("failed", "failed")])),
            "failed"
        );
        assert_eq!(
            summarize(
                &unit("inactive", "dead", "success"),
                &containers(&[("inactive", "dead")])
            ),
            "stopped"
        );
        assert_eq!(
            summarize(&unit("inactive", "dead", "exit-code"), &[]),
            "failed"
        );
        assert_eq!(summarize(&unit("inactive", "dead", ""), &[]), "stopped");
    }

    #[test]
    fn networks_nobody_names_are_unreferenced() {
        let network = |name: &str| NetworkRecord {
            name: name.into(),
            token: store::new_token(),
            created_at: String::new(),
        };
        let pod = |id: &str, network: Option<&str>| Record {
            id: id.into(),
            token: store::new_token(),
            spec: serde_json::from_value(serde_json::json!({
                "hostname": id,
                "containers": [{"name": "c", "image": "alpine"}],
                "network": network.map(|n| serde_json::json!({"name": n})),
            }))
            .unwrap(),
            desired: Desired::Running,
            error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let networks = [network("a"), network("b"), network("c")];
        let pods = [
            pod("p", Some("a")),
            pod("q", Some("a")),
            pod("r", None),
            pod("s", Some("c")),
        ];
        let names: Vec<&str> = unreferenced(&networks, &pods)
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, ["b"]);
        assert_eq!(unreferenced(&networks, &[]).len(), 3);
    }

    #[test]
    fn volumes_know_the_pods_that_mount_them() {
        let pod = |id: &str, volumes: serde_json::Value| Record {
            id: id.into(),
            token: store::new_token(),
            spec: serde_json::from_value(serde_json::json!({
                "hostname": id, "volumes": volumes,
                "containers": [{"name": "c", "image": "alpine"}],
            }))
            .unwrap(),
            desired: Desired::Running,
            error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let pods = [
            pod("a", serde_json::json!([{"name": "d", "volume": "data"}])),
            pod(
                "b",
                serde_json::json!([{"name": "d", "volume": "data"}, {"name": "s", "scratch": {"medium": "disk", "bytes": 1}}]),
            ),
            pod(
                "c",
                serde_json::json!([{"name": "data", "scratch": {"medium": "memory", "bytes": 1}}]),
            ),
        ];
        assert_eq!(mounting("data", &pods), ["a", "b"]);
        assert!(mounting("other", &pods).is_empty());
    }

    #[test]
    fn errors_map_to_http_statuses() {
        assert_eq!(
            WorkloadError::Invalid("x".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            WorkloadError::NotFound("x".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            WorkloadError::Conflict("x".into()).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            WorkloadError::Backend("x".into()).status(),
            StatusCode::BAD_GATEWAY
        );
    }
}
