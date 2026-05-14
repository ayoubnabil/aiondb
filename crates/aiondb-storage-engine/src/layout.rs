//! On-disk layout for the LSM checkpoint backend.
//!
//! # Manifest authority
//!
//! [`load_canonical_lsm_manifest`] resolves an apparent disagreement between
//! the manifest file and the run files actually present on disk. The rule is:
//!
//! * If the manifest is missing, or is present but empty (no runs and no
//!   `last_checkpoint_lsn`), the discovered run files become authoritative
//!   and the manifest is rewritten to match. This recovers from a crash that
//!   wrote the run + dir-fsynced it but never managed to publish a manifest
//!   referencing it.
//! * Otherwise the manifest is authoritative: missing referenced runs fail
//!   recovery, and unreferenced run files on disk are pruned by
//!   [`cleanup_unreferenced_runs`] after the manifest is rewritten.
//!
//! Run publication is two-phase: write `.tmp` → fsync → rename to the final
//! `<id>.sst` → fsync the parent directory. The manifest itself is updated
//! with the same tmp+rename+dirsync sequence so a crash mid-update never
//! leaves a half-written JSON file on disk.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};
use aiondb_storage_api::CheckpointInfo;
use serde::{Deserialize, Serialize};

use crate::lsm_sstable::{SSTableReader, SSTableWriter};

const LSM_MANIFEST_VERSION: u64 = 1;
const LSM_RUN_SUFFIX: &str = ".sst";
const LSM_LEGACY_RUN_VERSION: u64 = 1;
const LSM_LEGACY_RUN_KIND: &str = "checkpoint_stub";
const LSM_LEGACY_RUN_SUFFIX: &str = ".sst.json";
const LSM_LEVEL_ZERO_COMPACTION_THRESHOLD: usize = 4;
const MAX_LSM_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LEGACY_LSM_RUN_BYTES: u64 = 8 * 1024 * 1024;
const RUN_KEY_CHECKPOINT: &[u8] = b"checkpoint";
const RUN_KEY_SNAPSHOT_PREFIX: &str = "snapshot/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LsmLayout {
    pub base_dir: PathBuf,
    pub wal_dir: PathBuf,
    pub levels_dir: PathBuf,
    pub level_zero_dir: PathBuf,
    pub level_one_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub memtable_flush_bytes: usize,
    pub block_size_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct LsmManifest {
    version: u64,
    backend: String,
    memtable_flush_bytes: usize,
    block_size_bytes: usize,
    wal_dir: PathBuf,
    levels_dir: PathBuf,
    #[serde(default = "default_next_sstable_id")]
    next_sstable_id: u64,
    #[serde(default)]
    last_checkpoint_lsn: Option<u64>,
    #[serde(default)]
    level_zero_runs: Vec<LsmRunManifestEntry>,
    #[serde(default)]
    level_one_runs: Vec<LsmRunManifestEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct LsmRunManifestEntry {
    id: u64,
    level: u32,
    path: String,
    checkpoint_lsn: u64,
    dirty_pages_flushed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct LegacyLsmCheckpointRun {
    version: u64,
    backend: String,
    kind: String,
    id: u64,
    level: u32,
    checkpoint_lsn: u64,
    dirty_pages_flushed: u64,
}

fn default_next_sstable_id() -> u64 {
    1
}

pub(crate) fn prepare_lsm_layout(
    base_dir: &Path,
    memtable_flush_bytes: usize,
    block_size_bytes: usize,
) -> DbResult<LsmLayout> {
    let layout = LsmLayout {
        base_dir: base_dir.to_path_buf(),
        wal_dir: base_dir.join("wal"),
        levels_dir: base_dir.join("levels"),
        level_zero_dir: base_dir.join("levels").join("level-0"),
        level_one_dir: base_dir.join("levels").join("level-1"),
        manifest_path: base_dir.join("manifest.json"),
        memtable_flush_bytes,
        block_size_bytes,
    };

    fs::create_dir_all(&layout.base_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create lsm base directory {}: {error}",
            layout.base_dir.display()
        ))
    })?;
    fs::create_dir_all(&layout.wal_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create lsm wal directory {}: {error}",
            layout.wal_dir.display()
        ))
    })?;
    fs::create_dir_all(&layout.level_zero_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create lsm level directory {}: {error}",
            layout.level_zero_dir.display()
        ))
    })?;
    fs::create_dir_all(&layout.level_one_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create lsm level directory {}: {error}",
            layout.level_one_dir.display()
        ))
    })?;

    let manifest = load_canonical_lsm_manifest(&layout)?;
    write_lsm_manifest(&layout, &manifest)?;
    if let Err(error) = cleanup_unreferenced_runs(&layout, &manifest) {
        tracing::warn!("failed to prune unreferenced lsm runs during layout prepare: {error}");
    }

    Ok(layout)
}

pub(crate) fn record_lsm_checkpoint(
    layout: &LsmLayout,
    checkpoint: &CheckpointInfo,
    snapshot_bytes: &[u8],
) -> DbResult<PathBuf> {
    let mut manifest = load_canonical_lsm_manifest(layout)?;
    let run_id = manifest.next_sstable_id;
    let file_name = lsm_run_file_name(run_id);
    let run_path = layout.level_zero_dir.join(&file_name);
    write_lsm_checkpoint_segment(
        &run_path,
        checkpoint,
        snapshot_bytes,
        layout.block_size_bytes,
    )?;

    manifest.level_zero_runs.insert(
        0,
        LsmRunManifestEntry {
            id: run_id,
            level: 0,
            path: file_name,
            checkpoint_lsn: checkpoint.checkpoint_lsn,
            dirty_pages_flushed: checkpoint.dirty_pages_flushed,
        },
    );
    manifest.level_zero_runs.sort_by_key(|run| Reverse(run.id));
    manifest.next_sstable_id = run_id
        .checked_add(1)
        .ok_or_else(|| DbError::internal("lsm next_sstable_id overflowed u64"))?;
    compact_level_zero_runs(layout, &mut manifest)?;
    manifest.last_checkpoint_lsn = Some(checkpoint.checkpoint_lsn);
    write_lsm_manifest(layout, &manifest)?;
    if let Err(error) = cleanup_unreferenced_runs(layout, &manifest) {
        tracing::warn!("failed to prune unreferenced lsm runs after checkpoint: {error}");
    }

    Ok(run_path)
}

pub(crate) fn latest_snapshot_bytes(layout: &LsmLayout) -> DbResult<Option<Vec<u8>>> {
    let manifest = load_canonical_lsm_manifest(layout)?;
    let Some(run) = newest_run_entry(&manifest) else {
        return Ok(None);
    };
    read_run_snapshot_bytes(layout, run)
}

fn write_lsm_checkpoint_segment(
    path: &Path,
    checkpoint: &CheckpointInfo,
    snapshot_bytes: &[u8],
    block_size_bytes: usize,
) -> DbResult<()> {
    publish_sstable_atomically(path, block_size_bytes, |writer| {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&checkpoint.checkpoint_lsn.to_le_bytes());
        payload.extend_from_slice(&checkpoint.dirty_pages_flushed.to_le_bytes());
        writer.add(RUN_KEY_CHECKPOINT, Some(&payload))?;
        if snapshot_bytes.is_empty() {
            writer.add(snapshot_chunk_key(0).as_bytes(), Some(&[]))?;
        } else {
            for (chunk_index, chunk) in snapshot_bytes.chunks(block_size_bytes.max(1)).enumerate() {
                writer.add(snapshot_chunk_key(chunk_index).as_bytes(), Some(chunk))?;
            }
        }
        Ok(())
    })
}

fn load_canonical_lsm_manifest(layout: &LsmLayout) -> DbResult<LsmManifest> {
    let discovered_level_zero_runs = discover_runs_in_level(layout, 0)?;
    let discovered_level_one_runs = discover_runs_in_level(layout, 1)?;
    let manifest = if layout.manifest_path.exists() {
        let manifest = read_lsm_manifest(layout)?;
        validate_lsm_manifest(layout, &manifest)?;
        if manifest.level_zero_runs.is_empty()
            && manifest.level_one_runs.is_empty()
            && manifest.last_checkpoint_lsn.is_none()
        {
            canonicalize_discovered_lsm_manifest(
                layout,
                manifest,
                discovered_level_zero_runs,
                discovered_level_one_runs,
            )
        } else {
            validate_manifest_run_files_exist(layout, &manifest)?;
            canonicalize_existing_lsm_manifest(
                layout,
                manifest,
                &discovered_level_zero_runs,
                &discovered_level_one_runs,
            )
        }
    } else {
        canonicalize_discovered_lsm_manifest(
            layout,
            default_lsm_manifest(layout),
            discovered_level_zero_runs,
            discovered_level_one_runs,
        )
    };
    Ok(manifest)
}

fn default_lsm_manifest(layout: &LsmLayout) -> LsmManifest {
    LsmManifest {
        version: LSM_MANIFEST_VERSION,
        backend: "lsm".to_string(),
        memtable_flush_bytes: layout.memtable_flush_bytes,
        block_size_bytes: layout.block_size_bytes,
        wal_dir: layout.wal_dir.clone(),
        levels_dir: layout.levels_dir.clone(),
        next_sstable_id: default_next_sstable_id(),
        last_checkpoint_lsn: None,
        level_zero_runs: Vec::new(),
        level_one_runs: Vec::new(),
    }
}

fn canonicalize_discovered_lsm_manifest(
    layout: &LsmLayout,
    mut manifest: LsmManifest,
    discovered_level_zero_runs: Vec<LsmRunManifestEntry>,
    discovered_level_one_runs: Vec<LsmRunManifestEntry>,
) -> LsmManifest {
    manifest.version = LSM_MANIFEST_VERSION;
    manifest.backend = "lsm".to_string();
    manifest.memtable_flush_bytes = layout.memtable_flush_bytes;
    manifest.block_size_bytes = layout.block_size_bytes;
    manifest.wal_dir.clone_from(&layout.wal_dir);
    manifest.levels_dir.clone_from(&layout.levels_dir);
    manifest.level_zero_runs = discovered_level_zero_runs;
    manifest.level_one_runs = discovered_level_one_runs;
    manifest.level_zero_runs.sort_by_key(|run| Reverse(run.id));
    manifest.level_one_runs.sort_by_key(|run| Reverse(run.id));
    let discovered_next_sstable_id = manifest
        .level_zero_runs
        .iter()
        .chain(manifest.level_one_runs.iter())
        .map(|entry| entry.id)
        .max()
        .and_then(|id| id.checked_add(1))
        .unwrap_or_else(default_next_sstable_id);
    manifest.next_sstable_id = manifest
        .next_sstable_id
        .max(discovered_next_sstable_id)
        .max(default_next_sstable_id());
    if manifest.last_checkpoint_lsn.is_none() {
        manifest.last_checkpoint_lsn = manifest
            .level_zero_runs
            .iter()
            .chain(manifest.level_one_runs.iter())
            .max_by_key(|entry| entry.id)
            .map(|entry| entry.checkpoint_lsn);
    }
    manifest
}

fn canonicalize_existing_lsm_manifest(
    layout: &LsmLayout,
    mut manifest: LsmManifest,
    discovered_level_zero_runs: &[LsmRunManifestEntry],
    discovered_level_one_runs: &[LsmRunManifestEntry],
) -> LsmManifest {
    manifest.version = LSM_MANIFEST_VERSION;
    manifest.backend = "lsm".to_string();
    manifest.memtable_flush_bytes = layout.memtable_flush_bytes;
    manifest.block_size_bytes = layout.block_size_bytes;
    manifest.wal_dir.clone_from(&layout.wal_dir);
    manifest.levels_dir.clone_from(&layout.levels_dir);
    manifest.level_zero_runs.sort_by_key(|run| Reverse(run.id));
    manifest.level_one_runs.sort_by_key(|run| Reverse(run.id));
    let discovered_next_sstable_id = discovered_level_zero_runs
        .iter()
        .chain(discovered_level_one_runs.iter())
        .map(|entry| entry.id)
        .max()
        .and_then(|id| id.checked_add(1))
        .unwrap_or_else(default_next_sstable_id);
    manifest.next_sstable_id = manifest
        .next_sstable_id
        .max(discovered_next_sstable_id)
        .max(default_next_sstable_id());
    if manifest.last_checkpoint_lsn.is_none() {
        manifest.last_checkpoint_lsn = manifest
            .level_zero_runs
            .iter()
            .chain(manifest.level_one_runs.iter())
            .max_by_key(|entry| entry.id)
            .map(|entry| entry.checkpoint_lsn);
    }
    manifest
}

fn write_lsm_manifest(layout: &LsmLayout, manifest: &LsmManifest) -> DbResult<()> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| DbError::internal(format!("failed to encode lsm manifest: {error}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LSM_MANIFEST_BYTES {
        return Err(DbError::program_limit(format!(
            "lsm manifest exceeds maximum {MAX_LSM_MANIFEST_BYTES} bytes"
        )));
    }

    // Atomic write: tmp file -> fsync -> rename -> dir sync
    let tmp_path = layout.manifest_path.with_extension("json.tmp");
    let mut file = create_lsm_temp_file(&tmp_path, "tmp lsm manifest")?;
    file.write_all(&bytes)
        .map_err(|error| DbError::internal(format!("failed to write tmp lsm manifest: {error}")))?;
    file.sync_all()
        .map_err(|error| DbError::internal(format!("failed to fsync tmp lsm manifest: {error}")))?;
    fs::rename(&tmp_path, &layout.manifest_path).map_err(|error| {
        DbError::internal(format!(
            "failed to rename lsm manifest {}: {error}",
            layout.manifest_path.display()
        ))
    })?;
    sync_parent_dir(
        &layout.manifest_path,
        "failed to sync lsm manifest parent directory",
    )?;
    Ok(())
}

fn create_lsm_temp_file(path: &Path, context: &str) -> DbResult<File> {
    if path.exists() {
        fs::remove_file(path).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale {context} {}: {error}",
                path.display()
            ))
        })?;
        sync_parent_dir(
            path,
            "failed to sync parent directory after stale temp removal",
        )?;
    }

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create {context} {}: {error}",
                path.display()
            ))
        })
}

fn sync_dir(path: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(path)
        .map_err(|error| DbError::internal(format!("{context} {}: {error}", path.display())))
}

fn sync_parent_dir(path: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!("{context} {}: {error}", parent.display()))
    })
}

fn publish_sstable_atomically(
    final_path: &Path,
    block_size_bytes: usize,
    populate: impl FnOnce(&mut SSTableWriter) -> DbResult<()>,
) -> DbResult<()> {
    if final_path.exists() {
        return Err(DbError::internal(format!(
            "refusing to overwrite existing lsm run {}",
            final_path.display()
        )));
    }

    let tmp_path = temp_sstable_path(final_path);
    if tmp_path.exists() {
        fs::remove_file(&tmp_path).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale temporary lsm run {}: {error}",
                tmp_path.display()
            ))
        })?;
        sync_parent_dir(
            &tmp_path,
            "failed to sync parent directory for stale temporary lsm run",
        )?;
    }

    let publish_result = (|| -> DbResult<()> {
        let mut writer = SSTableWriter::create_with_block_size(&tmp_path, block_size_bytes)?;
        populate(&mut writer)?;
        writer.finish()?;
        fs::rename(&tmp_path, final_path).map_err(|error| {
            DbError::internal(format!(
                "failed to publish lsm run {}: {error}",
                final_path.display()
            ))
        })?;
        sync_parent_dir(
            final_path,
            "failed to sync parent directory for published lsm run",
        )
    })();

    if publish_result.is_err() && tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }

    publish_result
}

fn temp_sstable_path(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run.sst");
    final_path.with_file_name(format!(".{file_name}.tmp"))
}

fn read_lsm_manifest(layout: &LsmLayout) -> DbResult<LsmManifest> {
    let bytes = read_limited_layout_file(
        &layout.manifest_path,
        MAX_LSM_MANIFEST_BYTES,
        "lsm manifest",
    )?;
    serde_json::from_slice(&bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to parse lsm manifest {}: {error}",
            layout.manifest_path.display()
        ))
    })
}

fn validate_lsm_manifest(layout: &LsmLayout, manifest: &LsmManifest) -> DbResult<()> {
    if manifest.version != LSM_MANIFEST_VERSION {
        return Err(DbError::feature_not_supported(format!(
            "unsupported lsm manifest version {} in {}",
            manifest.version,
            layout.manifest_path.display()
        )));
    }

    if manifest.backend != "lsm" {
        return Err(DbError::internal(format!(
            "invalid lsm manifest {}: backend must be 'lsm', got '{}'",
            layout.manifest_path.display(),
            manifest.backend
        )));
    }

    if manifest.memtable_flush_bytes != layout.memtable_flush_bytes {
        return Err(DbError::feature_not_supported(format!(
            "lsm manifest {} expects memtable_flush_bytes={}, got {}",
            layout.manifest_path.display(),
            manifest.memtable_flush_bytes,
            layout.memtable_flush_bytes
        )));
    }

    if manifest.block_size_bytes != layout.block_size_bytes {
        return Err(DbError::feature_not_supported(format!(
            "lsm manifest {} expects block_size_bytes={}, got {}",
            layout.manifest_path.display(),
            manifest.block_size_bytes,
            layout.block_size_bytes
        )));
    }

    if manifest.wal_dir != layout.wal_dir {
        return Err(DbError::internal(format!(
            "lsm manifest {} expects wal_dir={}, got {}",
            layout.manifest_path.display(),
            manifest.wal_dir.display(),
            layout.wal_dir.display()
        )));
    }

    if manifest.levels_dir != layout.levels_dir {
        return Err(DbError::internal(format!(
            "lsm manifest {} expects levels_dir={}, got {}",
            layout.manifest_path.display(),
            manifest.levels_dir.display(),
            layout.levels_dir.display()
        )));
    }

    if manifest.next_sstable_id == 0 {
        return Err(DbError::internal(format!(
            "invalid lsm manifest {}: next_sstable_id must be >= 1",
            layout.manifest_path.display()
        )));
    }

    for run in &manifest.level_zero_runs {
        validate_manifest_run_entry(run, &layout.manifest_path)?;
    }
    for run in &manifest.level_one_runs {
        validate_manifest_run_entry(run, &layout.manifest_path)?;
    }

    Ok(())
}

fn discover_runs_in_level(layout: &LsmLayout, level: u32) -> DbResult<Vec<LsmRunManifestEntry>> {
    let level_dir = level_dir(layout, level)?;
    if !level_dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in fs::read_dir(&level_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to read lsm level directory {}: {error}",
            level_dir.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to read lsm level directory entry {}: {error}",
                level_dir.display()
            ))
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }

        let run = if file_name.ends_with(LSM_RUN_SUFFIX) {
            read_sstable_run(&path)?
        } else if level == 0 && file_name.ends_with(LSM_LEGACY_RUN_SUFFIX) {
            read_legacy_json_run(&path)?
        } else {
            continue;
        };

        validate_discovered_run_name(file_name, run.id, &path, level)?;
        runs.push(run.with_path(file_name, level));
    }

    runs.sort_by_key(|run| Reverse(run.id));
    Ok(runs)
}

fn read_sstable_run(path: &Path) -> DbResult<DiscoveredRun> {
    let reader = SSTableReader::open(path)?;
    let payload = reader
        .get(RUN_KEY_CHECKPOINT)?
        .ok_or_else(|| {
            DbError::internal(format!(
                "invalid lsm run {}: missing checkpoint entry",
                path.display()
            ))
        })?
        .ok_or_else(|| {
            DbError::internal(format!(
                "invalid lsm run {}: checkpoint entry must not be a tombstone",
                path.display()
            ))
        })?;
    if payload.len() != 16 {
        return Err(DbError::internal(format!(
            "invalid lsm run {}: checkpoint payload must be 16 bytes, got {}",
            path.display(),
            payload.len()
        )));
    }

    let checkpoint_lsn = u64::from_le_bytes(
        payload[0..8]
            .try_into()
            .map_err(|_| DbError::internal("lsm checkpoint payload decode failed"))?,
    );
    let dirty_pages_flushed = u64::from_le_bytes(
        payload[8..16]
            .try_into()
            .map_err(|_| DbError::internal("lsm checkpoint payload decode failed"))?,
    );

    Ok(DiscoveredRun {
        id: parse_run_id_from_name(path)?,
        checkpoint_lsn,
        dirty_pages_flushed,
    })
}

fn read_legacy_json_run(path: &Path) -> DbResult<DiscoveredRun> {
    let bytes = read_limited_layout_file(path, MAX_LEGACY_LSM_RUN_BYTES, "lsm level-0 run")?;
    let run: LegacyLsmCheckpointRun = serde_json::from_slice(&bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to parse lsm level-0 run {}: {error}",
            path.display()
        ))
    })?;
    if run.version != LSM_LEGACY_RUN_VERSION {
        return Err(DbError::feature_not_supported(format!(
            "unsupported lsm legacy run version {} in {}",
            run.version,
            path.display()
        )));
    }
    if run.backend != "lsm" {
        return Err(DbError::internal(format!(
            "invalid lsm legacy run {}: backend must be 'lsm', got '{}'",
            path.display(),
            run.backend
        )));
    }
    if run.kind != LSM_LEGACY_RUN_KIND {
        return Err(DbError::internal(format!(
            "invalid lsm legacy run {}: kind must be '{}', got '{}'",
            path.display(),
            LSM_LEGACY_RUN_KIND,
            run.kind
        )));
    }
    if run.level != 0 {
        return Err(DbError::internal(format!(
            "invalid lsm legacy run {}: level must be 0, got {}",
            path.display(),
            run.level
        )));
    }

    Ok(DiscoveredRun {
        id: run.id,
        checkpoint_lsn: run.checkpoint_lsn,
        dirty_pages_flushed: run.dirty_pages_flushed,
    })
}

fn read_limited_layout_file(path: &Path, max_bytes: u64, context: &str) -> DbResult<Vec<u8>> {
    let file = File::open(path).map_err(|error| {
        DbError::internal(format!(
            "failed to read {context} {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        DbError::internal(format!(
            "failed to inspect {context} {}: {error}",
            path.display()
        ))
    })?;
    if metadata.len() > max_bytes {
        return Err(DbError::program_limit(format!(
            "{context} file size exceeds maximum {max_bytes} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(max_bytes.saturating_add(1));
    reader.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to read {context} {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(DbError::program_limit(format!(
            "{context} file grew beyond maximum {max_bytes} bytes while reading"
        )));
    }
    Ok(bytes)
}

fn validate_manifest_run_entry(run: &LsmRunManifestEntry, manifest_path: &Path) -> DbResult<()> {
    if run.level > 1 {
        return Err(DbError::internal(format!(
            "invalid lsm manifest {}: run {} has unexpected level {}",
            manifest_path.display(),
            run.path,
            run.level
        )));
    }

    let expected_sstable_path = lsm_run_file_name(run.id);
    let expected_legacy_path = legacy_lsm_run_file_name(run.id);
    let path_matches =
        run.path == expected_sstable_path || (run.level == 0 && run.path == expected_legacy_path);
    if !path_matches {
        return Err(DbError::internal(format!(
            "invalid lsm manifest {}: level-{} run {} should be named {}{}",
            manifest_path.display(),
            run.level,
            run.path,
            expected_sstable_path,
            if run.level == 0 {
                format!(" or {expected_legacy_path}")
            } else {
                String::new()
            }
        )));
    }

    Ok(())
}

fn validate_manifest_run_files_exist(layout: &LsmLayout, manifest: &LsmManifest) -> DbResult<()> {
    for run in manifest
        .level_zero_runs
        .iter()
        .chain(manifest.level_one_runs.iter())
    {
        let path = run_path(layout, run);
        if !path.is_file() {
            return Err(DbError::internal(format!(
                "lsm manifest {} references missing run {}",
                layout.manifest_path.display(),
                path.display()
            )));
        }
    }
    Ok(())
}

fn cleanup_unreferenced_runs(layout: &LsmLayout, manifest: &LsmManifest) -> DbResult<()> {
    let referenced_runs: BTreeSet<PathBuf> = manifest
        .level_zero_runs
        .iter()
        .chain(manifest.level_one_runs.iter())
        .map(|run| run_path(layout, run))
        .collect();
    let mut synced_dirs = BTreeSet::new();

    for (level, dir) in [
        (0u32, &layout.level_zero_dir),
        (1u32, &layout.level_one_dir),
    ] {
        if !dir.is_dir() {
            continue;
        }

        for entry in fs::read_dir(dir).map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate lsm level directory {}: {error}",
                dir.display()
            ))
        })? {
            let entry = entry.map_err(|error| {
                DbError::internal(format!(
                    "failed to read lsm level directory entry {}: {error}",
                    dir.display()
                ))
            })?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|error| {
                    DbError::internal(format!(
                        "failed to inspect lsm level directory entry {}: {error}",
                        path.display()
                    ))
                })?
                .is_file()
            {
                continue;
            }

            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let is_recognized_run = file_name.ends_with(LSM_RUN_SUFFIX)
                || (level == 0 && file_name.ends_with(LSM_LEGACY_RUN_SUFFIX));
            if !is_recognized_run || referenced_runs.contains(&path) {
                continue;
            }

            fs::remove_file(&path).map_err(|error| {
                DbError::internal(format!(
                    "failed to remove obsolete lsm run {}: {error}",
                    path.display()
                ))
            })?;
            synced_dirs.insert(dir.clone());
        }
    }

    for dir in synced_dirs {
        sync_dir(&dir, "failed to sync lsm level directory after cleanup")?;
    }

    Ok(())
}

fn lsm_run_file_name(run_id: u64) -> String {
    format!("{run_id:020}{LSM_RUN_SUFFIX}")
}

fn snapshot_chunk_key(chunk_index: usize) -> String {
    format!("{RUN_KEY_SNAPSHOT_PREFIX}{chunk_index:020}")
}

fn legacy_lsm_run_file_name(run_id: u64) -> String {
    format!("{run_id:020}{LSM_LEGACY_RUN_SUFFIX}")
}

fn validate_discovered_run_name(
    file_name: &str,
    run_id: u64,
    path: &Path,
    level: u32,
) -> DbResult<()> {
    let matches = file_name == lsm_run_file_name(run_id)
        || (level == 0 && file_name == legacy_lsm_run_file_name(run_id));
    if !matches {
        return Err(DbError::internal(format!(
            "invalid lsm run {}: unexpected file name {}",
            path.display(),
            file_name
        )));
    }

    Ok(())
}

fn parse_run_id_from_name(path: &Path) -> DbResult<u64> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            DbError::internal(format!(
                "invalid lsm run {}: missing file name",
                path.display()
            ))
        })?;
    let stem = file_name
        .strip_suffix(LSM_RUN_SUFFIX)
        .or_else(|| file_name.strip_suffix(LSM_LEGACY_RUN_SUFFIX))
        .ok_or_else(|| {
            DbError::internal(format!(
                "invalid lsm run {}: unsupported file suffix",
                path.display()
            ))
        })?;

    stem.parse::<u64>().map_err(|error| {
        DbError::internal(format!(
            "invalid lsm run {}: failed to parse run id '{}': {error}",
            path.display(),
            stem
        ))
    })
}

fn compact_level_zero_runs(layout: &LsmLayout, manifest: &mut LsmManifest) -> DbResult<()> {
    if manifest.level_zero_runs.len() < LSM_LEVEL_ZERO_COMPACTION_THRESHOLD {
        return Ok(());
    }

    let newest_run = manifest
        .level_zero_runs
        .first()
        .cloned()
        .ok_or_else(|| DbError::internal("lsm compaction requires at least one level-0 run"))?;
    let compacted_run_id = manifest.next_sstable_id;
    let compacted_file_name = lsm_run_file_name(compacted_run_id);
    let compacted_path = layout.level_one_dir.join(&compacted_file_name);

    let mut merged_entries = BTreeMap::<Vec<u8>, Option<Vec<u8>>>::new();
    for run in manifest
        .level_zero_runs
        .iter()
        .chain(manifest.level_one_runs.iter())
    {
        for (key, value) in read_run_entries(layout, run)? {
            merged_entries.entry(key).or_insert(value);
        }
    }

    publish_sstable_atomically(&compacted_path, layout.block_size_bytes, |writer| {
        for (key, value) in &merged_entries {
            writer.add(key.as_slice(), value.as_deref())?;
        }
        Ok(())
    })?;

    manifest.level_zero_runs.clear();
    manifest.level_one_runs = vec![LsmRunManifestEntry {
        id: compacted_run_id,
        level: 1,
        path: compacted_file_name,
        checkpoint_lsn: newest_run.checkpoint_lsn,
        dirty_pages_flushed: newest_run.dirty_pages_flushed,
    }];
    manifest.last_checkpoint_lsn = Some(newest_run.checkpoint_lsn);
    manifest.next_sstable_id = compacted_run_id
        .checked_add(1)
        .ok_or_else(|| DbError::internal("lsm next_sstable_id overflowed u64"))?;

    Ok(())
}

fn read_run_entries(
    layout: &LsmLayout,
    run: &LsmRunManifestEntry,
) -> DbResult<Vec<(Vec<u8>, Option<Vec<u8>>)>> {
    let path = run_path(layout, run);
    if run.path.ends_with(LSM_RUN_SUFFIX) {
        return SSTableReader::open(&path)?.iter();
    }
    if run.level == 0 && run.path.ends_with(LSM_LEGACY_RUN_SUFFIX) {
        let legacy_run = read_legacy_json_run(&path)?;
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&legacy_run.checkpoint_lsn.to_le_bytes());
        payload.extend_from_slice(&legacy_run.dirty_pages_flushed.to_le_bytes());
        return Ok(vec![(RUN_KEY_CHECKPOINT.to_vec(), Some(payload))]);
    }

    Err(DbError::internal(format!(
        "unsupported lsm run format {}",
        path.display()
    )))
}

fn read_run_snapshot_bytes(
    layout: &LsmLayout,
    run: &LsmRunManifestEntry,
) -> DbResult<Option<Vec<u8>>> {
    if run.path.ends_with(LSM_LEGACY_RUN_SUFFIX) {
        return Ok(None);
    }

    let mut snapshot_chunks = BTreeMap::<usize, Vec<u8>>::new();
    for (key, value) in read_run_entries(layout, run)? {
        let Ok(decoded_key) = std::str::from_utf8(&key) else {
            continue;
        };
        let Some(chunk_suffix) = decoded_key.strip_prefix(RUN_KEY_SNAPSHOT_PREFIX) else {
            continue;
        };
        let chunk_index = chunk_suffix.parse::<usize>().map_err(|error| {
            DbError::internal(format!(
                "invalid lsm snapshot chunk key '{}' in run {}: {error}",
                decoded_key, run.path
            ))
        })?;
        let chunk = value.ok_or_else(|| {
            DbError::internal(format!(
                "invalid lsm snapshot chunk '{}' in run {}: tombstones are not supported",
                decoded_key, run.path
            ))
        })?;
        snapshot_chunks.insert(chunk_index, chunk);
    }

    if snapshot_chunks.is_empty() {
        return Ok(None);
    }

    let total_bytes = snapshot_chunks.values().map(Vec::len).sum();
    let mut snapshot_bytes = Vec::with_capacity(total_bytes);
    for chunk in snapshot_chunks.into_values() {
        snapshot_bytes.extend_from_slice(&chunk);
    }

    Ok(Some(snapshot_bytes))
}

fn newest_run_entry(manifest: &LsmManifest) -> Option<&LsmRunManifestEntry> {
    manifest
        .level_zero_runs
        .iter()
        .chain(manifest.level_one_runs.iter())
        .max_by_key(|run| run.id)
}

fn run_path(layout: &LsmLayout, run: &LsmRunManifestEntry) -> PathBuf {
    match run.level {
        0 => layout.level_zero_dir.join(&run.path),
        1 => layout.level_one_dir.join(&run.path),
        _ => layout
            .levels_dir
            .join(format!("level-{}", run.level))
            .join(&run.path),
    }
}

fn level_dir(layout: &LsmLayout, level: u32) -> DbResult<PathBuf> {
    match level {
        0 => Ok(layout.level_zero_dir.clone()),
        1 => Ok(layout.level_one_dir.clone()),
        _ => Ok(layout.levels_dir.join(format!("level-{level}"))),
    }
}

struct DiscoveredRun {
    id: u64,
    checkpoint_lsn: u64,
    dirty_pages_flushed: u64,
}

impl DiscoveredRun {
    fn with_path(self, path: &str, level: u32) -> LsmRunManifestEntry {
        LsmRunManifestEntry {
            id: self.id,
            level,
            path: path.to_string(),
            checkpoint_lsn: self.checkpoint_lsn,
            dirty_pages_flushed: self.dirty_pages_flushed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;

    #[test]
    fn prepare_lsm_layout_writes_checkpoint_defaults() {
        let base_dir = unique_temp_path("lsm-layout", "defaults");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");

        assert!(layout.level_zero_dir.is_dir());
        assert!(layout.level_one_dir.is_dir());

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 1);
        assert_eq!(manifest["last_checkpoint_lsn"], serde_json::Value::Null);
        assert_eq!(
            manifest["level_zero_runs"],
            serde_json::Value::Array(Vec::new())
        );
        assert_eq!(
            manifest["level_one_runs"],
            serde_json::Value::Array(Vec::new())
        );

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn record_lsm_checkpoint_creates_level_zero_run_and_advances_manifest() {
        let base_dir = unique_temp_path("lsm-layout", "checkpoint");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");

        let first_path = record_lsm_checkpoint(
            &layout,
            &CheckpointInfo {
                checkpoint_lsn: 42,
                dirty_pages_flushed: 7,
            },
            b"first checkpoint snapshot bytes",
        )
        .expect("first lsm checkpoint should record a level-0 run");
        let second_path = record_lsm_checkpoint(
            &layout,
            &CheckpointInfo {
                checkpoint_lsn: 84,
                dirty_pages_flushed: 11,
            },
            b"second checkpoint snapshot bytes",
        )
        .expect("second lsm checkpoint should record a new level-0 run");

        assert!(first_path.is_file());
        assert!(second_path.is_file());
        assert_ne!(first_path, second_path);
        assert_eq!(
            first_path.extension().and_then(|ext| ext.to_str()),
            Some("sst")
        );
        assert_eq!(
            second_path.extension().and_then(|ext| ext.to_str()),
            Some("sst")
        );

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 3);
        assert_eq!(manifest["last_checkpoint_lsn"], 84);
        assert_eq!(manifest["level_zero_runs"][0]["checkpoint_lsn"], 84);
        assert_eq!(manifest["level_zero_runs"][1]["checkpoint_lsn"], 42);
        let segment_entries = SSTableReader::open(&second_path)
            .expect("lsm checkpoint segment should open")
            .iter()
            .expect("lsm checkpoint segment should iterate");
        assert!(segment_entries
            .iter()
            .any(|(key, _)| key == RUN_KEY_CHECKPOINT));
        assert!(segment_entries.iter().any(|(key, _)| {
            std::str::from_utf8(key)
                .is_ok_and(|decoded| decoded.starts_with(RUN_KEY_SNAPSHOT_PREFIX))
        }));

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn discover_runs_in_higher_lsm_levels_returns_empty_when_directory_is_missing() {
        let base_dir = unique_temp_path("lsm-layout", "higher-level-discovery");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");

        let runs = discover_runs_in_level(&layout, 2)
            .expect("missing higher lsm level should behave like an empty level");
        assert!(runs.is_empty());

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn record_lsm_checkpoint_compacts_level_zero_runs_into_level_one() {
        let base_dir = unique_temp_path("lsm-layout", "compaction");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");
        let latest_snapshot = b"fourth checkpoint snapshot bytes";

        for (checkpoint_lsn, dirty_pages_flushed, snapshot_bytes) in [
            (42, 7, b"first checkpoint snapshot bytes".as_slice()),
            (84, 11, b"second checkpoint snapshot bytes".as_slice()),
            (126, 13, b"third checkpoint snapshot bytes".as_slice()),
            (168, 17, latest_snapshot.as_slice()),
        ] {
            record_lsm_checkpoint(
                &layout,
                &CheckpointInfo {
                    checkpoint_lsn,
                    dirty_pages_flushed,
                },
                snapshot_bytes,
            )
            .expect("lsm checkpoint should record a run");
        }

        let level_zero_runs: Vec<PathBuf> = std::fs::read_dir(&layout.level_zero_dir)
            .expect("level-0 directory should be readable")
            .map(|entry| entry.expect("level-0 entry should be readable").path())
            .collect();
        let level_one_runs: Vec<PathBuf> = std::fs::read_dir(&layout.level_one_dir)
            .expect("level-1 directory should be readable")
            .map(|entry| entry.expect("level-1 entry should be readable").path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
            .collect();
        assert!(level_zero_runs.is_empty());
        assert_eq!(level_one_runs.len(), 1);

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 6);
        assert_eq!(manifest["last_checkpoint_lsn"], 168);
        assert_eq!(
            manifest["level_zero_runs"],
            serde_json::Value::Array(Vec::new())
        );
        assert_eq!(manifest["level_one_runs"][0]["checkpoint_lsn"], 168);
        assert_eq!(manifest["level_one_runs"][0]["dirty_pages_flushed"], 17);

        let compacted_entries = SSTableReader::open(&level_one_runs[0])
            .expect("compacted sstable should open")
            .iter()
            .expect("compacted sstable should iterate");
        let checkpoint_payload = compacted_entries
            .iter()
            .find(|(key, _)| key == RUN_KEY_CHECKPOINT)
            .and_then(|(_, value)| value.as_ref())
            .expect("compacted run should retain checkpoint metadata");
        assert_eq!(checkpoint_payload.len(), 16);
        assert_eq!(
            u64::from_le_bytes(
                checkpoint_payload[0..8]
                    .try_into()
                    .expect("checkpoint lsn bytes should decode")
            ),
            168
        );
        let mut snapshot_chunks = compacted_entries
            .iter()
            .filter_map(|(key, value)| {
                let decoded_key = std::str::from_utf8(key).ok()?;
                let chunk_suffix = decoded_key.strip_prefix(RUN_KEY_SNAPSHOT_PREFIX)?;
                Some((
                    chunk_suffix
                        .parse::<usize>()
                        .expect("chunk suffix should decode"),
                    value
                        .clone()
                        .expect("snapshot chunks must not be tombstones"),
                ))
            })
            .collect::<Vec<_>>();
        snapshot_chunks.sort_by_key(|(index, _)| *index);
        let reconstructed_snapshot = snapshot_chunks
            .into_iter()
            .flat_map(|(_, chunk)| chunk)
            .collect::<Vec<_>>();
        assert_eq!(reconstructed_snapshot, latest_snapshot);

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_lsm_layout_recovers_discovered_runs_missing_from_manifest() {
        let base_dir = unique_temp_path("lsm-layout", "recovery");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");
        record_lsm_checkpoint(
            &layout,
            &CheckpointInfo {
                checkpoint_lsn: 128,
                dirty_pages_flushed: 9,
            },
            b"recovery checkpoint snapshot bytes",
        )
        .expect("checkpoint fixture should create a level-0 run");

        std::fs::write(
            &layout.manifest_path,
            format!(
                "{{\"version\":1,\"backend\":\"lsm\",\"memtable_flush_bytes\":4194304,\"block_size_bytes\":{},\"wal_dir\":\"{}\",\"levels_dir\":\"{}\",\"next_sstable_id\":1,\"last_checkpoint_lsn\":null,\"level_zero_runs\":[]}}",
                aiondb_buffer_pool::PAGE_SIZE,
                layout.wal_dir.display(),
                layout.levels_dir.display()
            ),
        )
        .expect("older manifest fixture should be writable");

        prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should recover discovered runs");

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 2);
        assert_eq!(manifest["last_checkpoint_lsn"], 128);
        assert_eq!(manifest["level_zero_runs"][0]["checkpoint_lsn"], 128);

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_lsm_layout_recovers_legacy_json_runs() {
        let base_dir = unique_temp_path("lsm-layout", "legacy-recovery");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");
        let legacy_path = layout.level_zero_dir.join(legacy_lsm_run_file_name(1));
        let legacy_run = LegacyLsmCheckpointRun {
            version: LSM_LEGACY_RUN_VERSION,
            backend: "lsm".to_string(),
            kind: LSM_LEGACY_RUN_KIND.to_string(),
            id: 1,
            level: 0,
            checkpoint_lsn: 256,
            dirty_pages_flushed: 13,
        };
        let legacy_bytes =
            serde_json::to_vec_pretty(&legacy_run).expect("legacy run fixture should encode");
        std::fs::write(&legacy_path, legacy_bytes).expect("legacy run fixture should be writable");
        std::fs::write(
            &layout.manifest_path,
            format!(
                "{{\"version\":1,\"backend\":\"lsm\",\"memtable_flush_bytes\":4194304,\"block_size_bytes\":{},\"wal_dir\":\"{}\",\"levels_dir\":\"{}\",\"next_sstable_id\":1,\"last_checkpoint_lsn\":null,\"level_zero_runs\":[]}}",
                aiondb_buffer_pool::PAGE_SIZE,
                layout.wal_dir.display(),
                layout.levels_dir.display()
            ),
        )
        .expect("older manifest fixture should be writable");

        prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should recover legacy runs");

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 2);
        assert_eq!(manifest["last_checkpoint_lsn"], 256);
        assert_eq!(
            manifest["level_zero_runs"][0]["path"],
            legacy_lsm_run_file_name(1)
        );

        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_lsm_layout_recovers_discovered_level_one_runs() {
        let base_dir = unique_temp_path("lsm-layout", "level-one-recovery");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");

        for checkpoint_lsn in [42, 84, 126, 168] {
            record_lsm_checkpoint(
                &layout,
                &CheckpointInfo {
                    checkpoint_lsn,
                    dirty_pages_flushed: checkpoint_lsn / 2,
                },
                format!("checkpoint-snapshot-{checkpoint_lsn}").as_bytes(),
            )
            .expect("checkpoint fixture should create compaction input");
        }

        std::fs::write(
            &layout.manifest_path,
            format!(
                "{{\"version\":1,\"backend\":\"lsm\",\"memtable_flush_bytes\":4194304,\"block_size_bytes\":{},\"wal_dir\":\"{}\",\"levels_dir\":\"{}\",\"next_sstable_id\":1,\"last_checkpoint_lsn\":null,\"level_zero_runs\":[],\"level_one_runs\":[]}}",
                aiondb_buffer_pool::PAGE_SIZE,
                layout.wal_dir.display(),
                layout.levels_dir.display()
            ),
        )
        .expect("older manifest fixture should be writable");

        prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should recover discovered level-1 runs");

        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 6);
        assert_eq!(manifest["last_checkpoint_lsn"], 168);
        assert_eq!(
            manifest["level_zero_runs"],
            serde_json::Value::Array(Vec::new())
        );
        assert_eq!(manifest["level_one_runs"][0]["checkpoint_lsn"], 168);
        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_lsm_layout_rejects_manifest_with_missing_referenced_run() {
        let base_dir = unique_temp_path("lsm-layout", "missing-manifest-run");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");
        let run_path = record_lsm_checkpoint(
            &layout,
            &CheckpointInfo {
                checkpoint_lsn: 42,
                dirty_pages_flushed: 7,
            },
            b"manifest-run-present",
        )
        .expect("checkpoint fixture should create a level-0 run");
        std::fs::remove_file(&run_path).expect("run fixture should be removable");

        let err = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect_err("manifest that references a missing run must fail recovery");
        assert!(err.to_string().contains("references missing run"));
        let _ = std::fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_lsm_layout_prunes_orphan_runs_when_manifest_is_authoritative() {
        let base_dir = unique_temp_path("lsm-layout", "prune-orphan-runs");
        let layout = prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("lsm layout should initialize");
        let retained_run = record_lsm_checkpoint(
            &layout,
            &CheckpointInfo {
                checkpoint_lsn: 42,
                dirty_pages_flushed: 5,
            },
            b"manifest-authoritative-snapshot",
        )
        .expect("checkpoint fixture should create the retained run");
        let orphan_path = layout.level_zero_dir.join(lsm_run_file_name(2));
        write_lsm_checkpoint_segment(
            &orphan_path,
            &CheckpointInfo {
                checkpoint_lsn: 84,
                dirty_pages_flushed: 9,
            },
            b"orphan-snapshot",
            aiondb_buffer_pool::PAGE_SIZE,
        )
        .expect("orphan run fixture should be writable");
        assert!(
            orphan_path.is_file(),
            "orphan run should exist before reopen"
        );
        prepare_lsm_layout(&base_dir, 4 * 1024 * 1024, aiondb_buffer_pool::PAGE_SIZE)
            .expect("authoritative manifest should reopen successfully");
        let manifest = std::fs::read_to_string(&layout.manifest_path)
            .expect("lsm manifest should be readable");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("lsm manifest should be valid json");
        assert_eq!(manifest["next_sstable_id"], 3);
        assert_eq!(manifest["last_checkpoint_lsn"], 42);
        assert_eq!(manifest["level_zero_runs"][0]["checkpoint_lsn"], 42);
        assert!(retained_run.is_file(), "referenced run must remain present");
        assert!(
            !orphan_path.exists(),
            "unreferenced orphan run should be pruned once manifest is authoritative"
        );
        let _ = std::fs::remove_dir_all(base_dir);
    }
}
