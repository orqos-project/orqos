//! Secret files (orqos#24): each secret volume of a pod is written under `/var/lib/orqos/workload/files/<pod>/<volume>`
//! (the parents are root-only) and bind-mounted read-only and idmapped, so the container sees the files' IDs
//! unchanged (owner root, the volume's group) whatever range the pod's user namespace gets.
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{chown, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::spec::{PodSpec, SecretFiles};
use super::WorkloadError;

/// Decoded data of the secrets a pod uses, by secret name.
pub type Secrets = BTreeMap<String, BTreeMap<String, Vec<u8>>>;

/// Directories get `r-x` wherever files get `r`: 0440 files sit in 0550 directories, 0644 files in 0555 ones.
pub fn directory_mode(mode: u32) -> u32 {
    let read = mode & 0o444;
    read | (read >> 2)
}

/// The host directory holding one pod's secret volumes.
pub fn pod_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

/// The bind-mount source of a secret volume, or of a path inside it.
pub fn source(root: &Path, id: &str, volume: &str, sub_path: Option<&str>) -> PathBuf {
    let dir = pod_dir(root, id).join(volume);
    match sub_path {
        Some(sub) => dir.join(sub),
        None => dir,
    }
}

fn io(context: &str, path: &Path) -> impl Fn(std::io::Error) -> WorkloadError {
    let what = format!("{context} {}", path.display());
    move |e| WorkloadError::Backend(format!("{what}: {e}"))
}

fn ensure_private(dir: &Path) -> Result<(), WorkloadError> {
    fs::create_dir_all(dir).map_err(io("create", dir))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io("chmod", dir))
}

fn make_dir(path: &Path, group: u32, mode: u32) -> Result<(), WorkloadError> {
    if !path.exists() {
        fs::create_dir(path).map_err(io("create", path))?;
    }
    chown(path, Some(0), Some(group)).map_err(io("chown", path))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io("chmod", path))
}

fn write_volume(
    dir: &Path,
    files: &SecretFiles,
    data: &BTreeMap<String, Vec<u8>>,
) -> Result<(), WorkloadError> {
    let group = files.group.unwrap_or(0);
    let dir_mode = directory_mode(files.mode);
    // Writable by root while it is filled; restricted at the end.
    make_dir(dir, group, 0o700)?;
    let mut directories = vec![dir.to_path_buf()];
    for (key, path) in files.files(data.keys().map(String::as_str)) {
        let value = data.get(key).ok_or_else(|| {
            WorkloadError::Invalid(format!("secret {:?} has no key {key:?}", files.name))
        })?;
        let target = dir.join(path);
        let mut parent = dir.to_path_buf();
        for component in Path::new(path)
            .parent()
            .into_iter()
            .flat_map(Path::components)
        {
            parent.push(component);
            if !directories.contains(&parent) {
                make_dir(&parent, group, 0o700)?;
                directories.push(parent.clone());
            }
        }
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&target)
            .map_err(io("create", &target))?;
        file.write_all(value).map_err(io("write", &target))?;
        file.sync_all().map_err(io("sync", &target))?;
        chown(&target, Some(0), Some(group)).map_err(io("chown", &target))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(files.mode))
            .map_err(io("chmod", &target))?;
    }
    // Deepest first, so restricting a directory never blocks reaching the ones below it.
    for directory in directories.iter().rev() {
        fs::set_permissions(directory, fs::Permissions::from_mode(dir_mode))
            .map_err(io("chmod", directory))?;
    }
    Ok(())
}

/// Writes every secret volume of the pod afresh: the new tree is built beside the old one and swapped in.
pub fn project(
    root: &Path,
    id: &str,
    spec: &PodSpec,
    secrets: &Secrets,
) -> Result<(), WorkloadError> {
    ensure_private(root)?;
    let target = pod_dir(root, id);
    let staged = root.join(format!(".{id}.new"));
    let old = root.join(format!(".{id}.old"));
    for leftover in [&staged, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).map_err(io("remove", leftover))?;
        }
    }
    if spec.secret_files().next().is_none() {
        return remove(root, id);
    }
    ensure_private(&staged)?;
    for (volume, files) in spec.secret_files() {
        let data = secrets
            .get(&files.name)
            .ok_or_else(|| WorkloadError::Invalid(format!("unknown secret {:?}", files.name)))?;
        write_volume(&staged.join(volume), files, data)?;
    }
    if target.exists() {
        fs::rename(&target, &old).map_err(io("move", &target))?;
    }
    fs::rename(&staged, &target).map_err(io("move", &staged))?;
    if old.exists() {
        fs::remove_dir_all(&old).map_err(io("remove", &old))?;
    }
    Ok(())
}

/// Whether every secret volume of the pod has been written.
pub fn present(root: &Path, id: &str, spec: &PodSpec) -> bool {
    spec.secret_files()
        .all(|(volume, _)| pod_dir(root, id).join(volume).is_dir())
}

/// Removes the pod's secret files.
pub fn remove(root: &Path, id: &str) -> Result<(), WorkloadError> {
    let target = pod_dir(root, id);
    if target.exists() {
        fs::remove_dir_all(&target).map_err(io("remove", &target))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_are_traversable_where_files_are_readable() {
        assert_eq!(directory_mode(0o440), 0o550);
        assert_eq!(directory_mode(0o644), 0o555);
        assert_eq!(directory_mode(0o400), 0o500);
        assert_eq!(directory_mode(0o600), 0o500);
        assert_eq!(directory_mode(0o000), 0o000);
        assert_eq!(directory_mode(0o777), 0o555);
    }

    #[test]
    fn sources_join_the_sub_path() {
        let root = Path::new("/var/lib/orqos/workload/files");
        assert_eq!(
            source(root, "shop", "tls", None),
            Path::new("/var/lib/orqos/workload/files/shop/tls")
        );
        assert_eq!(
            source(root, "shop", "tls", Some("a/b.pem")),
            Path::new("/var/lib/orqos/workload/files/shop/tls/a/b.pem")
        );
    }
}
