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

use spec::{valid_id, PodSpec};
use store::{Desired, Record, Store};
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

pub struct WorkloadState {
    token: String,
    paths: Paths,
    store: Store,
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
        Ok(Self {
            token,
            paths,
            store,
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
        if let Ownership::Foreign(why) = units::pod_ownership(labels.as_ref(), &record.token) {
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
        record.spec = spec;
        record.desired = Desired::Running;
        if changed {
            let files = render::render(id, &record.token, &record.spec);
            units::install(&self.paths, id, &record.token, &files).await?;
        }
        self.save(&mut record, None)?;
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
        let result = units::systemctl("start", &render::pod_unit(id)).await;
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
        self.store.remove(id).map_err(storage)?;
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
