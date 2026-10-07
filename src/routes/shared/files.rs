use std::{
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use axum::http::StatusCode;
use flate2::read::GzDecoder;
use tar::{Archive, Builder, Header};

type FileError = (StatusCode, String);

pub(crate) fn clean_path(raw: &str) -> Result<PathBuf, FileError> {
    let path = Path::new(raw);
    if !path.is_absolute() || raw.contains('\0') {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be absolute without NUL bytes".into(),
        ));
    }
    let mut result = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => result.push(name),
            Component::RootDir | Component::CurDir => {}
            _ => return Err((StatusCode::BAD_REQUEST, "path traversal not allowed".into())),
        }
    }
    Ok(result)
}

pub(crate) fn read_path(raw: &str, base: &Path) -> Result<PathBuf, FileError> {
    let target = clean_path(raw)?;
    if !target.starts_with(base) {
        return Err((
            StatusCode::FORBIDDEN,
            "path outside allowed base directory".into(),
        ));
    }
    if ["/etc", "/proc", "/sys", "/dev", "/var/run"]
        .iter()
        .any(|banned| target.starts_with(banned))
    {
        return Err((
            StatusCode::FORBIDDEN,
            "access to system dirs forbidden".into(),
        ));
    }
    Ok(target)
}

fn archive_error(error: std::io::Error) -> FileError {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

pub(crate) fn read_archive(bytes: Vec<u8>) -> Result<Vec<u8>, FileError> {
    let compressed = bytes.starts_with(&[0x1f, 0x8b]);
    let cursor = Cursor::new(bytes);
    let reader: Box<dyn Read> = if compressed {
        Box::new(GzDecoder::new(cursor))
    } else {
        Box::new(cursor)
    };
    let mut archive = Archive::new(reader);
    let mut entries = archive.entries().map_err(archive_error)?;
    let mut file = entries
        .next()
        .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?
        .map_err(archive_error)?;
    let kind = file.header().entry_type();
    if kind.is_symlink() || kind.is_hard_link() {
        return Err((StatusCode::FORBIDDEN, "links not allowed".into()));
    }
    if !kind.is_file() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be a regular file".into(),
        ));
    }
    // Entries share a cursor: consume the file before advancing the iterator.
    let mut content = Vec::new();
    file.read_to_end(&mut content).map_err(archive_error)?;
    if entries.next().is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must contain exactly one file".into(),
        ));
    }
    Ok(content)
}

pub(crate) fn write_archive(raw: &str, content: &[u8]) -> Result<(String, Vec<u8>), FileError> {
    let path = clean_path(raw)?;
    if raw.contains("/./") || raw.ends_with("/.") || raw.ends_with('/') {
        return Err((StatusCode::BAD_REQUEST, "path must name a file".into()));
    }
    let filename = path
        .file_name()
        .ok_or((StatusCode::BAD_REQUEST, "path must name a file".into()))?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    let mut builder = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, filename, Cursor::new(content))
        .map_err(archive_error)?;
    let bytes = builder.into_inner().map_err(archive_error)?;
    Ok((parent.to_string_lossy().into_owned(), bytes))
}

pub(crate) fn check_overwrite(exit_code: i64, stderr: &str) -> Result<(), FileError> {
    match exit_code {
        0 => Err((
            StatusCode::CONFLICT,
            "Refusing to overwrite existing file".into(),
        )),
        1 => Ok(()),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("existence check failed (exit {exit_code}): {stderr}"),
        )),
    }
}

pub(crate) fn check_command(command: &str, exit_code: i64, stderr: &str) -> Result<(), FileError> {
    if exit_code == 0 {
        Ok(())
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{command} failed (exit {exit_code}): {stderr}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    use tar::EntryType;

    fn fixture(kind: EntryType, content: &[u8], count: usize) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        for i in 0..count {
            let mut header = Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(0o644);
            header.set_size(content.len() as u64);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name("target").unwrap();
            }
            header.set_cksum();
            builder
                .append_data(&mut header, format!("file{i}"), content)
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn reads_exact_binary_and_empty_files() {
        for content in [b"audit".as_slice(), b"\0\xff\n", b""] {
            assert_eq!(
                read_archive(fixture(EntryType::Regular, content, 1)).unwrap(),
                content
            );
        }
    }

    #[test]
    fn reads_compressed_files() {
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&fixture(EntryType::Regular, b"audit", 1))
            .unwrap();
        assert_eq!(read_archive(gzip.finish().unwrap()).unwrap(), b"audit");
    }

    #[test]
    fn rejects_directories_links_and_multiple_entries() {
        for (kind, expected) in [
            (EntryType::Directory, StatusCode::BAD_REQUEST),
            (EntryType::Symlink, StatusCode::FORBIDDEN),
            (EntryType::Link, StatusCode::FORBIDDEN),
        ] {
            assert_eq!(read_archive(fixture(kind, b"", 1)).unwrap_err().0, expected);
        }
        assert_eq!(
            read_archive(fixture(EntryType::Regular, b"audit", 2))
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            read_archive(fixture(EntryType::Regular, b"", 0))
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn preserves_path_restrictions() {
        for path in ["relative", "/home/../etc/passwd", "/home/..", "/home/a\0b"] {
            assert_eq!(
                read_path(path, Path::new("/home")).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
        for path in [
            "/home-other/file",
            "/etc/passwd",
            "/proc/self/status",
            "/var/run/docker.sock",
        ] {
            assert_eq!(
                read_path(path, Path::new("/home")).unwrap_err().0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            read_path("/etc/passwd", Path::new("/")).unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            read_path("/home/./file", Path::new("/home")).unwrap(),
            Path::new("/home/file")
        );
    }

    #[test]
    fn uploads_basename_into_parent_directory() {
        let (parent, bytes) = write_archive("/home/upload.txt", b"audit").unwrap();
        assert_eq!(parent, "/home");
        let mut archive = Archive::new(Cursor::new(bytes));
        let mut entries = archive.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap(), Path::new("upload.txt"));
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"audit");
        assert!(entries.next().is_none());
        for path in ["/", "/home/", "/home/..", "/home/./file"] {
            assert!(write_archive(path, b"").is_err());
        }
    }

    #[test]
    fn checks_command_exit_status_instead_of_transport_success() {
        assert!(check_overwrite(1, "").is_ok());
        assert_eq!(check_overwrite(0, "").unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(
            check_overwrite(2, "denied").unwrap_err().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(check_command("chmod", 0, "").is_ok());
        assert!(check_command("chmod", 1, "invalid mode")
            .unwrap_err()
            .1
            .contains("invalid mode"));
        assert!(check_command("chown", -1, "").is_err());
    }
}
