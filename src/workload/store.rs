//! Pod records under `/var/lib/orqos/workload/<id>.json` (the accepted spec, its ownership token and the desired
//! state) and network records under `/var/lib/orqos/workload/networks/<name>.json`. Writes are atomic (temporary
//! file, fsync, rename, directory fsync).
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use utoipa::ToSchema;

use super::spec::{valid_id, valid_network_name, PodSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    /// 128-bit hex ownership token embedded in every unit, label and directory of the pod.
    pub token: String,
    pub spec: PodSpec,
    pub desired: Desired,
    /// Last failure of an operation on this pod, cleared by the next successful one.
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// An Orqos-owned Podman network (`orqos-net-<name>`), labelled with its own token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRecord {
    pub name: String,
    pub token: String,
    pub created_at: String,
}

/// A record stored as `<key>.json`.
pub trait Keyed: Serialize + DeserializeOwned {
    const WHAT: &'static str;
    fn valid_key(key: &str) -> bool;
    fn key(&self) -> &str;
    fn token(&self) -> &str;
}

impl Keyed for Record {
    const WHAT: &'static str = "pod id";
    fn valid_key(key: &str) -> bool {
        valid_id(key)
    }
    fn key(&self) -> &str {
        &self.id
    }
    fn token(&self) -> &str {
        &self.token
    }
}

impl Keyed for NetworkRecord {
    const WHAT: &'static str = "network name";
    fn valid_key(key: &str) -> bool {
        valid_network_name(key)
    }
    fn key(&self) -> &str {
        &self.name
    }
    fn token(&self) -> &str {
        &self.token
    }
}

pub fn new_token() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn is_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub struct Store<T = Record> {
    dir: PathBuf,
    // Held for the daemon's lifetime: a second Orqos process cannot manage the same records.
    _lock: File,
    _records: std::marker::PhantomData<T>,
}

impl<T: Keyed> Store<T> {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let meta = fs::symlink_metadata(dir)?;
        anyhow::ensure!(
            meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o077 == 0,
            "{} must be a root-owned directory with mode 0700",
            dir.display()
        );
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(dir.join(".lock"))?;
        lock.try_lock().map_err(|_| {
            anyhow::anyhow!(
                "another Orqos process manages workloads in {}",
                dir.display()
            )
        })?;
        Ok(Self {
            dir: dir.into(),
            _lock: lock,
            _records: std::marker::PhantomData,
        })
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn load(&self, id: &str) -> anyhow::Result<Option<T>> {
        anyhow::ensure!(T::valid_key(id), "invalid {}", T::WHAT);
        match fs::read(self.path(id)) {
            Ok(bytes) => {
                let record: T = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    record.key() == id && is_token(record.token()),
                    "corrupt record {id}"
                );
                Ok(Some(record))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self) -> anyhow::Result<Vec<T>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().into_string().unwrap_or_default();
            if let Some(id) = name.strip_suffix(".json") {
                if T::valid_key(id) {
                    records.extend(self.load(id)?);
                }
            }
        }
        records.sort_by(|a, b| a.key().cmp(b.key()));
        Ok(records)
    }

    pub fn save(&self, record: &T) -> anyhow::Result<()> {
        let path = self.path(record.key());
        let temporary = self.dir.join(format!(".{}.json.new", record.key()));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(record)?)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        fs::remove_file(self.path(id))?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_32_lowercase_hex() {
        let token = new_token();
        assert!(is_token(&token), "{token}");
        assert_ne!(token, new_token());
        assert!(!is_token("0123"));
        assert!(!is_token(&"G".repeat(32)));
    }
}
