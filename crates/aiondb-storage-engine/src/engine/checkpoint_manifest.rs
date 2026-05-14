//! Disk-checkpoint manifest with retained generations.
//!
//! Each checkpoint publishes:
//!
//! 1. A new `generations/lsn_<N>/` directory containing a self-contained
//!    copy of the snapshot file + paged artifacts at LSN `N`.
//! 2. An updated `manifest.json` that records `checkpoint_lsn = N` and the
//!    most recent `DISK_CHECKPOINT_GENERATION_RETENTION` generations,
//!    newest first.
//!
//! Recovery in `recover_disk_checkpoint_snapshot_bytes` prefers the
//! generation matching `manifest.checkpoint_lsn` over the live snapshot
//! files in the data directory; the live files are kept as a compatibility
//! fallback for layouts written before generations existed and as a
//! belt-and-braces source if a generation is unreadable. Older generations
//! beyond the retention limit are pruned only after the new manifest has
//! been published, so a crash mid-rotation cannot strand the database
//! without a recoverable snapshot.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use aiondb_core::{checksum::compute_crc32c, DbError, DbResult};
use aiondb_wal::Lsn;
use serde::{Deserialize, Serialize};

use super::{snapshot, PagedTableStore};

const DISK_CHECKPOINT_MANIFEST_FILENAME: &str = "manifest.json";
const DISK_CHECKPOINT_MANIFEST_TMP_FILENAME: &str = "manifest.json.tmp";
const DISK_CHECKPOINT_MANIFEST_MAGIC: &[u8; 8] = b"AIONCKP1";
const DISK_CHECKPOINT_GENERATIONS_DIR: &str = "generations";
const DISK_CHECKPOINT_PAGED_SNAPSHOT_DIR: &str = "pages";
const DISK_CHECKPOINT_TABLE_PAGES_DIR: &str = "table_pages";
const DISK_CHECKPOINT_MANIFEST_VERSION: u64 = 1;
const DISK_CHECKPOINT_GENERATION_RETENTION: usize = 3;
const MAX_DISK_CHECKPOINT_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DiskCheckpointGeneration {
    checkpoint_lsn: u64,
    snapshot_dir: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DiskCheckpointManifest {
    version: u64,
    backend: String,
    checkpoint_lsn: u64,
    file_snapshot_present: bool,
    paged_snapshot_present: bool,
    paged_tables_checkpoint_lsn: Option<u64>,
    #[serde(default)]
    generations: Vec<DiskCheckpointGeneration>,
}

pub(super) fn publish_disk_checkpoint_manifest(
    dir: &Path,
    checkpoint_lsn: Lsn,
    snapshot_bytes: &[u8],
    file_snapshot_present: bool,
    paged_snapshot_present: bool,
    paged_tables: Option<&PagedTableStore>,
) -> DbResult<()> {
    let paged_tables_checkpoint_lsn = paged_tables
        .map(PagedTableStore::current_checkpoint_lsn)
        .transpose()?
        .flatten();
    if paged_tables.is_some() && paged_tables_checkpoint_lsn != Some(checkpoint_lsn) {
        return Err(DbError::internal(format!(
            "disk checkpoint manifest expected paged tables at LSN {}, got {:?}",
            checkpoint_lsn.get(),
            paged_tables_checkpoint_lsn.map(|lsn| lsn.get())
        )));
    }

    let mut generations = load_disk_checkpoint_manifest(dir)?
        .map(|manifest| manifest.generations)
        .unwrap_or_default();
    let generation = publish_disk_checkpoint_generation(dir, checkpoint_lsn, snapshot_bytes)?;
    generations.retain(|entry| entry.checkpoint_lsn != checkpoint_lsn.get());
    generations.insert(0, generation);
    generations.truncate(DISK_CHECKPOINT_GENERATION_RETENTION);

    write_disk_checkpoint_manifest(
        dir,
        &DiskCheckpointManifest {
            version: DISK_CHECKPOINT_MANIFEST_VERSION,
            backend: "disk".to_string(),
            checkpoint_lsn: checkpoint_lsn.get(),
            file_snapshot_present,
            paged_snapshot_present,
            paged_tables_checkpoint_lsn: paged_tables_checkpoint_lsn.map(Lsn::get),
            generations: generations.clone(),
        },
    )?;
    prune_disk_checkpoint_generations(dir, &generations)
}

pub(super) fn recover_disk_checkpoint_snapshot_bytes(
    dir: &Path,
    snapshot_frames: usize,
    max_open_files: usize,
) -> DbResult<Option<Vec<u8>>> {
    let Some(manifest) = load_disk_checkpoint_manifest(dir)? else {
        return legacy_checkpoint_snapshot_bytes(dir, snapshot_frames, max_open_files);
    };

    if let Some(generation) = manifest
        .generations
        .iter()
        .find(|generation| generation.checkpoint_lsn == manifest.checkpoint_lsn)
    {
        if let Some(snapshot_bytes) =
            recover_disk_checkpoint_generation_snapshot_bytes(dir, generation)?
        {
            return Ok(Some(snapshot_bytes));
        }
    }

    let expected_lsn = Lsn::new(manifest.checkpoint_lsn);
    if manifest.file_snapshot_present {
        if let Some(snapshot_bytes) =
            load_snapshot_if_matching(expected_lsn, || super::snapshot_file_bytes(dir))
        {
            return Ok(Some(snapshot_bytes));
        }
    }
    if manifest.paged_snapshot_present {
        if let Some(snapshot_bytes) = load_snapshot_if_matching(expected_lsn, || {
            super::paged_snapshot_bytes(dir, snapshot_frames, max_open_files)
        }) {
            return Ok(Some(snapshot_bytes));
        }
    }

    Err(DbError::internal(format!(
        "disk checkpoint manifest expected a recoverable snapshot at LSN {}, but none could be loaded",
        expected_lsn.get()
    )))
}

fn legacy_checkpoint_snapshot_bytes(
    dir: &Path,
    snapshot_frames: usize,
    max_open_files: usize,
) -> DbResult<Option<Vec<u8>>> {
    if let Some(snapshot_bytes) = load_valid_snapshot(|| super::snapshot_file_bytes(dir)) {
        return Ok(Some(snapshot_bytes));
    }
    Ok(load_valid_snapshot(|| {
        super::paged_snapshot_bytes(dir, snapshot_frames, max_open_files)
    }))
}

fn snapshot_bytes_checkpoint_lsn(snapshot_bytes: &[u8]) -> DbResult<Lsn> {
    snapshot::deserialize_snapshot_bytes(snapshot_bytes).map(|(header, _)| header.checkpoint_lsn)
}

fn publish_disk_checkpoint_generation(
    dir: &Path,
    checkpoint_lsn: Lsn,
    snapshot_bytes: &[u8],
) -> DbResult<DiskCheckpointGeneration> {
    let generations_dir = disk_checkpoint_generations_dir(dir);
    fs::create_dir_all(&generations_dir).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint generations could not create directory {}: {error}",
            generations_dir.display()
        ))
    })?;
    sync_dir(dir, "disk checkpoint root directory")?;

    let snapshot_dir = format!("lsn_{}", checkpoint_lsn.get());
    let generation_dir = generations_dir.join(&snapshot_dir);
    snapshot::write_snapshot_file(snapshot_bytes, &generation_dir)?;
    copy_generation_paged_artifacts(dir, &generation_dir)?;
    sync_dir(&generations_dir, "disk checkpoint generations directory")?;

    Ok(DiskCheckpointGeneration {
        checkpoint_lsn: checkpoint_lsn.get(),
        snapshot_dir,
    })
}

fn recover_disk_checkpoint_generation_snapshot_bytes(
    dir: &Path,
    generation: &DiskCheckpointGeneration,
) -> DbResult<Option<Vec<u8>>> {
    let generation_dir = disk_checkpoint_generation_dir(dir, generation)?;
    let Some(snapshot_bytes) =
        load_snapshot_if_matching(Lsn::new(generation.checkpoint_lsn), || {
            super::snapshot_file_bytes(&generation_dir)
        })
    else {
        return Ok(None);
    };
    restore_generation_paged_artifacts(dir, &generation_dir)?;
    Ok(Some(snapshot_bytes))
}

fn prune_disk_checkpoint_generations(
    dir: &Path,
    generations: &[DiskCheckpointGeneration],
) -> DbResult<()> {
    let generations_dir = disk_checkpoint_generations_dir(dir);
    if !generations_dir.is_dir() {
        return Ok(());
    }

    let retained: BTreeSet<String> = generations
        .iter()
        .map(normalized_generation_dir_name)
        .collect::<DbResult<_>>()?;

    for entry in fs::read_dir(&generations_dir).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint generations could not enumerate {}: {error}",
            generations_dir.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "disk checkpoint generations could not read entry in {}: {error}",
                generations_dir.display()
            ))
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                DbError::internal(format!(
                    "disk checkpoint generations could not inspect {}: {error}",
                    entry.path().display()
                ))
            })?
            .is_dir()
        {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if retained.contains(&name) {
            continue;
        }

        fs::remove_dir_all(entry.path()).map_err(|error| {
            DbError::internal(format!(
                "disk checkpoint generations could not remove obsolete {}: {error}",
                entry.path().display()
            ))
        })?;
    }

    sync_dir(&generations_dir, "disk checkpoint generations directory")
}

fn load_snapshot_if_matching(
    expected_lsn: Lsn,
    load: impl FnOnce() -> DbResult<Option<Vec<u8>>>,
) -> Option<Vec<u8>> {
    let (snapshot_bytes, checkpoint_lsn) = load_valid_snapshot_with_lsn(load)?;
    (checkpoint_lsn == expected_lsn).then_some(snapshot_bytes)
}

fn load_valid_snapshot(load: impl FnOnce() -> DbResult<Option<Vec<u8>>>) -> Option<Vec<u8>> {
    load_valid_snapshot_with_lsn(load).map(|(snapshot_bytes, _)| snapshot_bytes)
}

fn load_valid_snapshot_with_lsn(
    load: impl FnOnce() -> DbResult<Option<Vec<u8>>>,
) -> Option<(Vec<u8>, Lsn)> {
    let Ok(Some(snapshot_bytes)) = load() else {
        return None;
    };
    let checkpoint_lsn = snapshot_bytes_checkpoint_lsn(&snapshot_bytes).ok()?;
    Some((snapshot_bytes, checkpoint_lsn))
}

fn copy_generation_paged_artifacts(dir: &Path, generation_dir: &Path) -> DbResult<()> {
    copy_generation_artifact_if_present(
        &dir.join(DISK_CHECKPOINT_PAGED_SNAPSHOT_DIR),
        &generation_dir.join(DISK_CHECKPOINT_PAGED_SNAPSHOT_DIR),
        "disk checkpoint generation paged snapshot",
    )?;
    copy_generation_artifact_if_present(
        &dir.join(DISK_CHECKPOINT_TABLE_PAGES_DIR),
        &generation_dir.join(DISK_CHECKPOINT_TABLE_PAGES_DIR),
        "disk checkpoint generation paged tables",
    )
}

fn copy_generation_artifact_if_present(
    source: &Path,
    target: &Path,
    context: &str,
) -> DbResult<()> {
    if !source.is_dir() {
        return Ok(());
    }
    copy_dir_tree_durable(source, target, context)
}

fn restore_generation_paged_artifacts(dir: &Path, generation_dir: &Path) -> DbResult<()> {
    restore_generation_artifact_dir(
        &generation_dir.join(DISK_CHECKPOINT_PAGED_SNAPSHOT_DIR),
        &dir.join(DISK_CHECKPOINT_PAGED_SNAPSHOT_DIR),
        "disk checkpoint recovery paged snapshot",
    )?;
    restore_generation_artifact_dir(
        &generation_dir.join(DISK_CHECKPOINT_TABLE_PAGES_DIR),
        &dir.join(DISK_CHECKPOINT_TABLE_PAGES_DIR),
        "disk checkpoint recovery paged tables",
    )
}

fn restore_generation_artifact_dir(source: &Path, target: &Path, context: &str) -> DbResult<()> {
    let staging_dir = restoration_staging_dir(target)?;
    remove_path_if_exists(&staging_dir, context)?;

    if !source.is_dir() {
        remove_path_if_exists(target, context)?;
        return Ok(());
    }

    remove_path_if_exists(target, context)?;
    copy_dir_tree_durable(source, &staging_dir, context)?;
    fs::rename(&staging_dir, target).map_err(|error| {
        DbError::internal(format!(
            "{context} could not publish {}: {error}",
            target.display()
        ))
    })?;
    sync_parent_dir(target, context)?;
    Ok(())
}

fn remove_path_if_exists(path: &Path, context: &str) -> DbResult<()> {
    // Try remove_dir_all first (handles both files and directories on most
    // platforms).  If that fails because it is a plain file, fall back to
    // remove_file.  NotFound is always tolerated to avoid a TOCTOU race
    // between an existence check and the actual removal.
    match fs::remove_dir_all(path) {
        Ok(()) => return sync_parent_dir(path, context),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {}
    }
    match fs::remove_file(path) {
        Ok(()) => sync_parent_dir(path, context),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DbError::internal(format!(
            "{context} could not remove {}: {error}",
            path.display()
        ))),
    }
}

fn restoration_staging_dir(target: &Path) -> DbResult<PathBuf> {
    let Some(file_name) = target.file_name() else {
        return Err(DbError::internal(format!(
            "disk checkpoint recovery target {} has no file name",
            target.display()
        )));
    };
    Ok(target.with_file_name(format!(".{}.restore_tmp", file_name.to_string_lossy())))
}

const MAX_CHECKPOINT_DIR_COPY_DEPTH: usize = 256;

fn copy_dir_tree_durable(source: &Path, target: &Path, context: &str) -> DbResult<()> {
    copy_dir_tree_durable_at_depth(source, target, context, 0)
}

fn copy_dir_tree_durable_at_depth(
    source: &Path,
    target: &Path,
    context: &str,
    depth: usize,
) -> DbResult<()> {
    if depth >= MAX_CHECKPOINT_DIR_COPY_DEPTH {
        return Err(DbError::program_limit(format!(
            "{context} directory depth exceeds limit {MAX_CHECKPOINT_DIR_COPY_DEPTH}"
        )));
    }
    ensure_durable_dir(target, context)?;

    for entry in fs::read_dir(source).map_err(|error| {
        DbError::internal(format!(
            "{context} could not enumerate source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "{context} could not enumerate source {}: {error}",
                source.display()
            ))
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "{context} could not stat source {}: {error}",
                source_path.display()
            ))
        })?;

        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "{context} refusing symlink {}",
                source_path.display()
            )));
        }

        if file_type.is_dir() {
            copy_dir_tree_durable_at_depth(&source_path, &target_path, context, depth + 1)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                DbError::internal(format!(
                    "{context} could not copy file {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                ))
            })?;
            sync_file(&target_path, context)?;
        }
    }

    sync_dir(target, context)?;
    Ok(())
}

fn ensure_durable_dir(dir: &Path, context: &str) -> DbResult<()> {
    fs::create_dir_all(dir).map_err(|error| {
        DbError::internal(format!(
            "{context} could not create directory {}: {error}",
            dir.display()
        ))
    })?;
    sync_dir(dir, context)?;
    sync_parent_dir(dir, context)
}

fn sync_file(path: &Path, context: &str) -> DbResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "{context} could not sync file {}: {error}",
                path.display()
            ))
        })
}

fn sync_parent_dir(path: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "{context} sync failed for {}: {error}",
            parent.display()
        ))
    })
}

fn disk_checkpoint_generations_dir(dir: &Path) -> PathBuf {
    dir.join(DISK_CHECKPOINT_GENERATIONS_DIR)
}

fn disk_checkpoint_generation_dir(
    dir: &Path,
    generation: &DiskCheckpointGeneration,
) -> DbResult<PathBuf> {
    Ok(disk_checkpoint_generations_dir(dir).join(normalized_generation_dir_name(generation)?))
}

fn normalized_generation_dir_name(generation: &DiskCheckpointGeneration) -> DbResult<String> {
    let mut components = Path::new(&generation.snapshot_dir).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) => Ok(component.to_string_lossy().into_owned()),
        _ => Err(DbError::internal(format!(
            "disk checkpoint generation path must be a single relative directory name, got '{}'",
            generation.snapshot_dir
        ))),
    }
}

fn write_disk_checkpoint_manifest(dir: &Path, manifest: &DiskCheckpointManifest) -> DbResult<()> {
    fs::create_dir_all(dir).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not create directory {}: {error}",
            dir.display()
        ))
    })?;

    let manifest_path = disk_checkpoint_manifest_path(dir);
    let tmp_path = dir.join(DISK_CHECKPOINT_MANIFEST_TMP_FILENAME);
    let manifest_json = serde_json::to_vec_pretty(manifest).map_err(|error| {
        DbError::internal(format!("disk checkpoint manifest encode failed: {error}"))
    })?;
    let manifest_bytes = frame_disk_checkpoint_manifest(&manifest_json);

    let mut file = create_disk_checkpoint_manifest_temp_file(&tmp_path)?;
    file.write_all(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not write temp file {}: {error}",
            tmp_path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not flush temp file {}: {error}",
            tmp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not sync temp file {}: {error}",
            tmp_path.display()
        ))
    })?;
    drop(file);

    fs::rename(&tmp_path, &manifest_path).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not publish {}: {error}",
            manifest_path.display()
        ))
    })?;
    sync_dir(dir, "disk checkpoint manifest directory")?;
    Ok(())
}

fn load_disk_checkpoint_manifest(dir: &Path) -> DbResult<Option<DiskCheckpointManifest>> {
    let manifest_path = disk_checkpoint_manifest_path(dir);
    if !manifest_path.is_file() {
        return Ok(None);
    }

    let bytes = read_disk_checkpoint_manifest_file(&manifest_path)?;
    let manifest = decode_disk_checkpoint_manifest_bytes(&manifest_path, &bytes)?;
    if manifest.version != DISK_CHECKPOINT_MANIFEST_VERSION {
        return Err(DbError::internal(format!(
            "disk checkpoint manifest version must be {}, got {}",
            DISK_CHECKPOINT_MANIFEST_VERSION, manifest.version
        )));
    }
    if manifest.backend != "disk" {
        return Err(DbError::internal(format!(
            "disk checkpoint manifest backend must be 'disk', got '{}'",
            manifest.backend
        )));
    }
    Ok(Some(manifest))
}

fn create_disk_checkpoint_manifest_temp_file(tmp_path: &Path) -> DbResult<File> {
    if tmp_path.exists() {
        fs::remove_file(tmp_path).map_err(|error| {
            DbError::internal(format!(
                "disk checkpoint manifest could not clear stale temp file {}: {error}",
                tmp_path.display()
            ))
        })?;
        if let Some(parent) = tmp_path.parent() {
            sync_dir(parent, "disk checkpoint manifest directory")?;
        }
    }

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .map_err(|error| {
            DbError::internal(format!(
                "disk checkpoint manifest could not create temp file {}: {error}",
                tmp_path.display()
            ))
        })
}

fn read_disk_checkpoint_manifest_file(manifest_path: &Path) -> DbResult<Vec<u8>> {
    let file = File::open(manifest_path).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not read {}: {error}",
            manifest_path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not inspect {}: {error}",
            manifest_path.display()
        ))
    })?;
    if metadata.len() > MAX_DISK_CHECKPOINT_MANIFEST_BYTES {
        return Err(DbError::program_limit(format!(
            "disk checkpoint manifest {} exceeds maximum {} bytes",
            manifest_path.display(),
            MAX_DISK_CHECKPOINT_MANIFEST_BYTES
        )));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAX_DISK_CHECKPOINT_MANIFEST_BYTES.saturating_add(1));
    reader.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not read {}: {error}",
            manifest_path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DISK_CHECKPOINT_MANIFEST_BYTES {
        return Err(DbError::program_limit(format!(
            "disk checkpoint manifest {} grew beyond maximum {} bytes while reading",
            manifest_path.display(),
            MAX_DISK_CHECKPOINT_MANIFEST_BYTES
        )));
    }

    Ok(bytes)
}

fn frame_disk_checkpoint_manifest(manifest_json: &[u8]) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(DISK_CHECKPOINT_MANIFEST_MAGIC.len() + 8 + manifest_json.len() + 4);
    bytes.extend_from_slice(DISK_CHECKPOINT_MANIFEST_MAGIC);
    bytes.extend_from_slice(&(manifest_json.len() as u64).to_le_bytes());
    bytes.extend_from_slice(manifest_json);
    let checksum = compute_crc32c(&bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_disk_checkpoint_manifest_bytes(
    manifest_path: &Path,
    bytes: &[u8],
) -> DbResult<DiskCheckpointManifest> {
    if bytes.starts_with(DISK_CHECKPOINT_MANIFEST_MAGIC) {
        let min_len = DISK_CHECKPOINT_MANIFEST_MAGIC.len() + 8 + 4;
        if bytes.len() < min_len {
            return Err(DbError::internal(format!(
                "disk checkpoint manifest {} is truncated",
                manifest_path.display()
            )));
        }
        let checksum_offset = bytes.len() - 4;
        let stored = u32::from_le_bytes(
            bytes[checksum_offset..]
                .try_into()
                .map_err(|_| DbError::internal("disk checkpoint manifest checksum truncated"))?,
        );
        let computed = compute_crc32c(&bytes[..checksum_offset]);
        if stored != computed {
            return Err(DbError::internal(format!(
                "disk checkpoint manifest {} checksum mismatch",
                manifest_path.display()
            )));
        }
        let payload_len = u64::from_le_bytes(
            bytes[DISK_CHECKPOINT_MANIFEST_MAGIC.len()..DISK_CHECKPOINT_MANIFEST_MAGIC.len() + 8]
                .try_into()
                .map_err(|_| DbError::internal("disk checkpoint manifest length truncated"))?,
        );
        let payload_len = usize::try_from(payload_len).map_err(|_| {
            DbError::internal(format!(
                "disk checkpoint manifest {} payload length overflows usize",
                manifest_path.display()
            ))
        })?;
        let payload_start = DISK_CHECKPOINT_MANIFEST_MAGIC.len() + 8;
        let payload_end = payload_start.checked_add(payload_len).ok_or_else(|| {
            DbError::internal(format!(
                "disk checkpoint manifest {} payload length overflow",
                manifest_path.display()
            ))
        })?;
        if payload_end + 4 != bytes.len() {
            return Err(DbError::internal(format!(
                "disk checkpoint manifest {} payload length mismatch",
                manifest_path.display()
            )));
        }
        return serde_json::from_slice(&bytes[payload_start..payload_end]).map_err(|error| {
            DbError::internal(format!(
                "disk checkpoint manifest could not decode {}: {error}",
                manifest_path.display()
            ))
        });
    }

    serde_json::from_slice(bytes).map_err(|error| {
        DbError::internal(format!(
            "disk checkpoint manifest could not decode legacy JSON {}: {error}",
            manifest_path.display()
        ))
    })
}

fn disk_checkpoint_manifest_path(dir: &Path) -> PathBuf {
    dir.join(DISK_CHECKPOINT_MANIFEST_FILENAME)
}

fn sync_dir(dir: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(dir).map_err(|error| {
        DbError::internal(format!(
            "{context} sync failed for {}: {error}",
            dir.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_fixture() -> DiskCheckpointManifest {
        DiskCheckpointManifest {
            version: DISK_CHECKPOINT_MANIFEST_VERSION,
            backend: "disk".to_owned(),
            checkpoint_lsn: 42,
            file_snapshot_present: true,
            paged_snapshot_present: false,
            paged_tables_checkpoint_lsn: Some(42),
            generations: vec![DiskCheckpointGeneration {
                checkpoint_lsn: 42,
                snapshot_dir: "lsn_42".to_owned(),
            }],
        }
    }

    #[test]
    fn framed_disk_checkpoint_manifest_roundtrips() {
        let manifest = manifest_fixture();
        let json = serde_json::to_vec(&manifest).expect("manifest JSON");
        let framed = frame_disk_checkpoint_manifest(&json);

        assert!(framed.starts_with(DISK_CHECKPOINT_MANIFEST_MAGIC));
        let decoded = decode_disk_checkpoint_manifest_bytes(Path::new("manifest.json"), &framed)
            .expect("framed manifest should decode");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn legacy_json_disk_checkpoint_manifest_still_decodes() {
        let manifest = manifest_fixture();
        let json = serde_json::to_vec(&manifest).expect("manifest JSON");

        let decoded = decode_disk_checkpoint_manifest_bytes(Path::new("manifest.json"), &json)
            .expect("legacy JSON manifest should decode");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn framed_disk_checkpoint_manifest_rejects_bad_checksum() {
        let manifest = manifest_fixture();
        let json = serde_json::to_vec(&manifest).expect("manifest JSON");
        let mut framed = frame_disk_checkpoint_manifest(&json);
        let last = framed.last_mut().expect("checksum byte");
        *last ^= 0xff;

        let err = decode_disk_checkpoint_manifest_bytes(Path::new("manifest.json"), &framed)
            .expect_err("bad checksum must fail");
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn load_disk_checkpoint_manifest_rejects_oversized_file() {
        let dir = crate::test_support::unique_temp_path("disk-checkpoint-manifest", "oversized");
        std::fs::create_dir_all(&dir).expect("manifest dir should be creatable");
        let manifest_path = disk_checkpoint_manifest_path(&dir);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&manifest_path)
            .expect("oversized manifest should be creatable");
        file.set_len(MAX_DISK_CHECKPOINT_MANIFEST_BYTES + 1)
            .expect("oversized manifest length should be settable");

        let err =
            load_disk_checkpoint_manifest(&dir).expect_err("oversized manifest must be rejected");
        assert!(
            err.to_string().contains("exceeds maximum"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
