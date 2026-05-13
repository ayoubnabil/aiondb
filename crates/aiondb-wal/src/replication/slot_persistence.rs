use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};

use crate::lsn::Lsn;
use crate::replication::PhysicalReplicationSlot;
use crate::segment;

const MAX_SLOT_NAME_BYTES: usize = 63;
const MAX_SLOT_METADATA_BYTES: u64 = 16 * 1024;

pub(super) fn validate_slot_name(name: &str) -> DbResult<()> {
    if name.is_empty()
        || name.len() > MAX_SLOT_NAME_BYTES
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(DbError::internal(format!(
            "invalid physical replication slot name: {name}"
        )));
    }
    Ok(())
}

pub(super) fn slot_metadata_path(slot_dir: &Path, slot_name: &str) -> PathBuf {
    slot_dir.join(format!("{slot_name}.json"))
}

pub(super) fn read_slot_file(path: &Path) -> DbResult<PhysicalReplicationSlot> {
    let file = fs::File::open(path).map_err(|error| {
        DbError::internal(format!(
            "failed to open replication slot metadata {}: {error}",
            path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to stat replication slot metadata {}: {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > MAX_SLOT_METADATA_BYTES {
        return Err(DbError::internal(format!(
            "replication slot metadata {} exceeds maximum size of {MAX_SLOT_METADATA_BYTES} bytes",
            path.display()
        )));
    }
    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "replication slot metadata {} length does not fit in usize",
            path.display()
        ))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(MAX_SLOT_METADATA_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to read replication slot metadata {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SLOT_METADATA_BYTES {
        return Err(DbError::internal(format!(
            "replication slot metadata {} grew beyond maximum size of {MAX_SLOT_METADATA_BYTES} bytes while reading",
            path.display()
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to decode replication slot metadata {}: {error}",
            path.display()
        ))
    })?;
    let name = value
        .get("slot_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            DbError::internal(format!(
                "replication slot metadata {} is missing slot_name",
                path.display()
            ))
        })?;
    if let Some(version) = value.get("version") {
        if version.as_u64() != Some(1) {
            return Err(DbError::internal(format!(
                "replication slot metadata {} has unsupported version",
                path.display()
            )));
        }
    }
    if let Some(slot_type) = value.get("slot_type") {
        if slot_type.as_str() != Some("physical") {
            return Err(DbError::internal(format!(
                "replication slot metadata {} has unsupported slot_type",
                path.display()
            )));
        }
    }
    validate_slot_name(name)?;
    let restart_lsn = optional_u64_field(&value, "restart_lsn", path)?.map(Lsn::new);
    let restart_tli = optional_u64_field(&value, "restart_tli", path)?.unwrap_or(1);
    Ok(PhysicalReplicationSlot {
        name: name.to_owned(),
        restart_lsn,
        restart_tli,
    })
}

fn optional_u64_field(
    value: &serde_json::Value,
    field: &str,
    path: &Path,
) -> DbResult<Option<u64>> {
    match value.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(raw) => raw.as_u64().map(Some).ok_or_else(|| {
            DbError::internal(format!(
                "replication slot metadata {} has invalid {field}",
                path.display()
            ))
        }),
    }
}

pub(super) fn write_slot_file_atomically(
    path: &Path,
    slot: &PhysicalReplicationSlot,
) -> DbResult<()> {
    let temp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 1u64,
        "slot_name": slot.name.as_str(),
        "slot_type": "physical",
        "restart_lsn": slot.restart_lsn.map(Lsn::get),
        "restart_tli": slot.restart_tli,
    }))
    .map_err(|error| {
        DbError::internal(format!(
            "failed to encode replication slot metadata {}: {error}",
            path.display()
        ))
    })?;
    let mut temp_file = create_slot_temp_file(&temp_path)?;
    temp_file.write_all(&bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to write replication slot metadata temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync replication slot metadata temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(temp_file);
    fs::rename(&temp_path, path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish replication slot metadata {}: {error}",
            path.display()
        ))
    })?;
    sync_parent_dir(path, "replication slot metadata")
}

fn create_slot_temp_file(path: &Path) -> DbResult<fs::File> {
    for attempt in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                fs::remove_file(path).map_err(|remove_error| {
                    DbError::internal(format!(
                        "failed to cleanup stale replication slot temp file {}: {remove_error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to create replication slot temp file {}: {error}",
                    path.display()
                )));
            }
        }
    }

    Err(DbError::internal(
        "failed to create replication slot temp file",
    ))
}

pub(super) fn sync_dir(path: &Path) -> DbResult<()> {
    segment::sync_dir(path).map_err(|error| {
        DbError::internal(format!(
            "failed to sync replication slot directory {}: {error}",
            path.display()
        ))
    })
}

pub(super) fn sync_parent_dir(path: &Path, context: &str) -> DbResult<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    sync_dir(parent).map_err(|error| {
        DbError::internal(format!(
            "failed to sync {context} parent directory {}: {error}",
            parent.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "aiondb-slot-persistence-test-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn read_slot_file_rejects_oversized_metadata() {
        let dir = unique_test_dir("oversized");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slot.json");
        fs::write(&path, vec![b' '; MAX_SLOT_METADATA_BYTES as usize + 1]).unwrap();

        let error = read_slot_file(&path).expect_err("oversized metadata must be rejected");
        assert!(error.to_string().contains("exceeds maximum size"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_slot_file_atomically_replaces_stale_temp_file() {
        let dir = unique_test_dir("stale-temp");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("standby1.json");
        let temp_path = path.with_extension("json.tmp");
        fs::write(&temp_path, b"stale").unwrap();

        let slot = PhysicalReplicationSlot {
            name: "standby1".to_owned(),
            restart_lsn: Some(Lsn::new(42)),
            restart_tli: 1,
        };
        write_slot_file_atomically(&path, &slot).unwrap();

        assert!(!temp_path.exists());
        assert_eq!(read_slot_file(&path).unwrap(), slot);

        fs::remove_dir_all(&dir).ok();
    }
}
