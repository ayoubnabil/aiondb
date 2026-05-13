//! Filesystem helpers shared by the storage-engine and catalog-store
//! replication seed installers.
//!
//! These helpers were previously copy-pasted in
//! `aiondb-storage-engine::replication` and
//! `aiondb-catalog-store::replication`. They share identical bodies (same
//! semantics, same error messages) so they live here as a single source of
//! truth. Both call sites still own their seed-export pipeline; only the
//! small leaf utilities are unified.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{DbError, DbResult};

/// Build a unique sibling staging-directory path for atomic seed installs.
///
/// The returned path lives in the same parent as `target_root` and is
/// disambiguated by both the current process id and a wall-clock nanosecond
/// timestamp, so concurrent installers don't collide.
#[must_use]
pub fn staging_dir_path(target_root: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let file_name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("target");
    let staging_name = format!(".{file_name}.seed-install-{}-{nanos}", std::process::id());
    target_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(staging_name)
}

/// Reject a non-empty target directory before installing a replication seed.
///
/// Missing targets are accepted; the caller is expected to create them.
///
/// # Errors
///
/// Returns an internal error when the directory listing cannot be read or
/// when the directory has at least one entry.
pub fn ensure_target_absent_or_empty(path: &Path, context: &str) -> DbResult<()> {
    if !path.exists() {
        return Ok(());
    }

    let mut entries = fs::read_dir(path).map_err(|error| {
        DbError::internal(format!(
            "failed to inspect {context} target {}: {error}",
            path.display()
        ))
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to inspect {context} target {}: {error}",
                path.display()
            ))
        })?
        .is_some()
    {
        return Err(DbError::internal(format!(
            "{context} target {} must be empty",
            path.display()
        )));
    }
    Ok(())
}

/// Convert a relative path to its replication-manifest representation
/// (forward-slash separated, regardless of host OS).
#[must_use]
pub fn relative_to_manifest_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Read a file in full, but bail out if its size exceeds `max_bytes`.
///
/// Used to guard against malicious or corrupted seed manifests that would
/// otherwise drive a multi-gigabyte allocation.
///
/// # Errors
///
/// Returns an internal error when metadata or read fails, or when the file
/// exceeds the configured cap.
pub fn read_file_capped(path: &Path, context: &str, max_bytes: u64) -> DbResult<Vec<u8>> {
    let file = fs::File::open(path).map_err(|error| {
        DbError::internal(format!(
            "failed to open {context} {}: {error}",
            path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to read {context} metadata {}: {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > max_bytes {
        return Err(DbError::internal(format!(
            "{context} {} is {file_len} bytes, exceeding maximum {max_bytes} bytes",
            path.display()
        )));
    }
    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "{context} {} size {file_len} does not fit in usize",
            path.display()
        ))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(max_bytes.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to read {context} {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(DbError::internal(format!(
            "{context} {} grew while reading, exceeding maximum {max_bytes} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "aiondb-replication-fs-test-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn read_file_capped_reads_small_file() {
        let path = unique_test_file("small");
        fs::write(&path, b"ok").unwrap();

        let bytes = read_file_capped(&path, "test file", 16).unwrap();
        assert_eq!(bytes, b"ok");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_file_capped_rejects_oversized_file() {
        let path = unique_test_file("oversized");
        fs::write(&path, b"toolong").unwrap();

        let error = read_file_capped(&path, "test file", 3).expect_err("oversized file must fail");
        assert!(error.to_string().contains("exceeding maximum"));

        fs::remove_file(&path).ok();
    }
}
