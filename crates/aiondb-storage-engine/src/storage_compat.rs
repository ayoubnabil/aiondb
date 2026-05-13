//! Storage format contract, doctor, and disk upgrade helpers.
//!
//! Product releases can stay on the `0.1` line; this module tracks the
//! persistent storage format separately. The current stable contract is disk
//! format v1 for catalog snapshots, SQL table heap/page files, WAL segments,
//! and primary/ordered index pages. Graph, vector HNSW, and distributed state
//! are intentionally reported as experimental artifacts.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_buffer_pool::PAGE_SIZE;
use aiondb_core::checksum::{compute_crc32c, compute_legacy_fnv1a};
use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::StorageBackendKind;

pub const STORAGE_FORMAT_MAJOR: u16 = 1;
pub const STORAGE_FORMAT_MINOR: u16 = 0;
pub const MIN_READABLE_STORAGE_FORMAT_MAJOR: u16 = 1;
pub const MAX_READABLE_STORAGE_FORMAT_MAJOR: u16 = 1;

const MANIFEST_FILE: &str = "aiondb.storage";
const MANIFEST_MAGIC: &[u8; 8] = b"AIONFMT1";
const MAX_STORAGE_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const DATA_MAGIC: &[u8; 9] = b"AIONDATA1";
const LEGACY_DATA_MAGIC: &[u8; 9] = b"AION_SNP\x01";
const CATALOG_MAGIC: &[u8; 9] = b"AIONCAT1\0";
const LEGACY_CATALOG_MAGIC: &[u8; 9] = b"AION_CAT\x01";
const WAL_MAGIC: &[u8; 8] = b"AIONWAL1";
const LEGACY_WAL_MAGIC: &[u8; 8] = b"AIONWAL\0";
const HEAP_PAGE_MAGIC: &[u8; 8] = b"AIONHP01";
const BTREE_META_MAGIC: &[u8; 8] = b"AIONBTM1";
const BTREE_PAGE_MAGIC: &[u8; 8] = b"AIONBTB1";
const VAR_BTREE_META_MAGIC: &[u8; 8] = b"AIONVTM1";
const VAR_BTREE_LEAF_MAGIC: &[u8; 8] = b"AIONVTL1";
const VAR_BTREE_INTERNAL_MAGIC: &[u8; 8] = b"AIONVTI1";
const PAGED_TABLE_MAGIC: &[u8; 8] = b"AIONTPG2";
const PAGED_SNAPSHOT_HEADER_MAGIC: &[u8; 8] = b"AIONSP02";
const PAGED_SNAPSHOT_PUBLISHED_MAGIC: &[u8; 8] = b"AIONSPM1";
const PAGED_SNAPSHOT_HEADER_RELATION_ID: u64 = u64::MAX - 1;
const PAGED_SNAPSHOT_SLOT_RELATION_IDS: [u64; 2] = [u64::MAX - 2, u64::MAX - 3];
const PAGED_SNAPSHOT_PUBLISHED_RELATION_ID: u64 = u64::MAX - 4;
const FPW_JOURNAL_FILENAME: &str = "fpw_journal.bin";
const FPW_JOURNAL_MAGIC: &[u8; 8] = b"AIONFPW1";
const FPW_JOURNAL_RECORD_BYTES: usize = FPW_JOURNAL_MAGIC.len() + 8 + 8 + PAGE_SIZE + 4;
const CHECKPOINT_MANIFEST_FILENAME: &str = "manifest.json";
const CHECKPOINT_MANIFEST_MAGIC: &[u8; 8] = b"AIONCKP1";
const DISK_CHECKPOINT_MANIFEST_VERSION: u64 = 1;
const CHECKSUM_SUFFIX: &str = ".csum";
const MAX_DOCTOR_CHECKSUM_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageFileKind {
    Heap,
    Index,
    Mixed,
    PagedTable,
    PagedSnapshot,
    Empty,
    Corrupt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StorageManifest {
    format_major: u16,
    format_minor: u16,
    created_by_release_line: String,
    backend: String,
    stable: Vec<String>,
    experimental: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StorageDoctorReport {
    pub data_dir: PathBuf,
    pub format_major: Option<u16>,
    pub format_minor: Option<u16>,
    pub manifest_present: bool,
    pub stable_files: usize,
    pub wal_segments: usize,
    pub catalog_snapshots: usize,
    pub storage_snapshots: usize,
    pub paged_table_files: usize,
    pub fpw_journals: usize,
    pub checkpoint_manifests: usize,
    pub heap_page_files: usize,
    pub index_page_files: usize,
    pub mixed_page_files: usize,
    pub empty_page_files: usize,
    pub experimental_files: usize,
    pub stable_paths: Vec<String>,
    pub experimental_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub upgrade_possible: bool,
}

impl StorageDoctorReport {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    #[must_use]
    pub fn upgrade_status(&self) -> &'static str {
        if self.upgrade_possible {
            "possible"
        } else {
            "refused"
        }
    }
}

pub fn ensure_storage_contract_for_open(
    data_dir: &Path,
    backend: StorageBackendKind,
) -> DbResult<()> {
    if matches!(backend, StorageBackendKind::InMemory) {
        return Ok(());
    }
    fs::create_dir_all(data_dir).map_err(|error| {
        DbError::internal(format!(
            "storage compatibility: failed to create data-dir {}: {error}",
            data_dir.display()
        ))
    })?;

    let manifest_path = manifest_path(data_dir);
    if manifest_path.is_file() {
        let manifest = read_manifest(data_dir)?;
        ensure_manifest_readable(&manifest)
    } else if data_dir_has_stable_artifacts(data_dir)? {
        Err(DbError::internal(format!(
            "storage compatibility: data-dir {} has stable disk files but no {}; run `aiondb upgrade --data-dir {}` before opening it",
            data_dir.display(),
            MANIFEST_FILE,
            data_dir.display()
        )))
    } else {
        write_manifest(data_dir, backend)
    }
}

pub fn doctor_data_dir(data_dir: &Path) -> StorageDoctorReport {
    let mut report = StorageDoctorReport {
        data_dir: data_dir.to_path_buf(),
        format_major: None,
        format_minor: None,
        manifest_present: false,
        stable_files: 0,
        wal_segments: 0,
        catalog_snapshots: 0,
        storage_snapshots: 0,
        paged_table_files: 0,
        fpw_journals: 0,
        checkpoint_manifests: 0,
        heap_page_files: 0,
        index_page_files: 0,
        mixed_page_files: 0,
        empty_page_files: 0,
        experimental_files: 0,
        stable_paths: Vec::new(),
        experimental_paths: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
        upgrade_possible: false,
    };

    if !data_dir.exists() {
        report
            .errors
            .push(format!("data-dir does not exist: {}", data_dir.display()));
        return report;
    }
    if !data_dir.is_dir() {
        report.errors.push(format!(
            "data-dir is not a directory: {}",
            data_dir.display()
        ));
        return report;
    }

    match read_manifest(data_dir) {
        Ok(manifest) => {
            report.manifest_present = true;
            report.format_major = Some(manifest.format_major);
            report.format_minor = Some(manifest.format_minor);
            if let Err(error) = ensure_manifest_readable(&manifest) {
                report.errors.push(error.to_string());
            }
        }
        Err(_) if !manifest_path(data_dir).exists() => {
            report
                .warnings
                .push(format!("missing {MANIFEST_FILE}; upgrade can create it"));
        }
        Err(error) => report.errors.push(error.to_string()),
    }

    inspect_tree(data_dir, data_dir, &mut report);
    if let Err(error) =
        aiondb_wal::segment::verify_local_wal_dir_if_configured(&data_dir.join("wal"))
    {
        report.errors.push(error.to_string());
    }
    report.upgrade_possible = upgrade_is_possible(&report);
    report
}

pub fn upgrade_data_dir(data_dir: &Path) -> DbResult<PathBuf> {
    let report = doctor_data_dir(data_dir);
    if !report.upgrade_possible {
        return Err(DbError::internal(format!(
            "storage upgrade refused for {}: {}",
            data_dir.display(),
            if report.errors.is_empty() {
                "state is ambiguous"
            } else {
                "doctor reported errors"
            }
        )));
    }
    if report.manifest_present
        && report.format_major == Some(STORAGE_FORMAT_MAJOR)
        && report.format_minor == Some(STORAGE_FORMAT_MINOR)
    {
        return Ok(manifest_path(data_dir));
    }

    let backup_dir = backup_data_dir(data_dir)?;
    write_manifest(data_dir, StorageBackendKind::Durable)?;
    Ok(backup_dir)
}

fn manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MANIFEST_FILE)
}

fn current_manifest(backend: StorageBackendKind) -> StorageManifest {
    StorageManifest {
        format_major: STORAGE_FORMAT_MAJOR,
        format_minor: STORAGE_FORMAT_MINOR,
        created_by_release_line: "0.1".to_owned(),
        backend: backend.as_str().to_owned(),
        stable: vec![
            "catalog snapshots and catalog WAL".to_owned(),
            "SQL table heap/page files".to_owned(),
            "WAL segments".to_owned(),
            "primary and ordered indexes".to_owned(),
        ],
        experimental: vec![
            "vector HNSW indexes".to_owned(),
            "graph labels and adjacency accelerators".to_owned(),
            "distributed/HA metadata".to_owned(),
        ],
    }
}

fn write_manifest(data_dir: &Path, backend: StorageBackendKind) -> DbResult<()> {
    fs::create_dir_all(data_dir).map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to create {}: {error}",
            data_dir.display()
        ))
    })?;
    let payload = serde_json::to_vec_pretty(&current_manifest(backend)).map_err(|error| {
        DbError::internal(format!("storage manifest serialize failed: {error}"))
    })?;
    let mut bytes = Vec::with_capacity(MANIFEST_MAGIC.len() + 8 + payload.len() + 4);
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&payload);
    let checksum = compute_crc32c(&bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());

    let path = manifest_path(data_dir);
    let tmp = data_dir.join(format!("{MANIFEST_FILE}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|error| {
            DbError::internal(format!(
                "storage manifest: failed to create {}: {error}",
                tmp.display()
            ))
        })?;
    file.write_all(&bytes).map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to write {}: {error}",
            tmp.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to sync {}: {error}",
            tmp.display()
        ))
    })?;
    drop(file);
    fs::rename(&tmp, &path).map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to publish {}: {error}",
            path.display()
        ))
    })?;
    aiondb_wal::segment::sync_dir(data_dir)
}

fn read_manifest(data_dir: &Path) -> DbResult<StorageManifest> {
    let path = manifest_path(data_dir);
    let data = read_storage_manifest_bytes(&path)?;
    if data.len() < MANIFEST_MAGIC.len() + 8 + 4 {
        return Err(DbError::internal("storage manifest: file too small"));
    }
    if &data[..MANIFEST_MAGIC.len()] != MANIFEST_MAGIC {
        return Err(DbError::internal("storage manifest: invalid magic"));
    }
    let checksum_offset = data.len() - 4;
    let stored = u32::from_le_bytes(read_fixed(&data, checksum_offset, "checksum")?);
    let computed = compute_crc32c(&data[..checksum_offset]);
    if stored != computed && stored != compute_legacy_fnv1a(&data[..checksum_offset]) {
        return Err(DbError::internal("storage manifest: checksum mismatch"));
    }
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&data[MANIFEST_MAGIC.len()..MANIFEST_MAGIC.len() + 8]);
    let payload_len = usize::try_from(u64::from_le_bytes(len_bytes))
        .map_err(|_| DbError::internal("storage manifest: payload length overflow"))?;
    let payload_start = MANIFEST_MAGIC.len() + 8;
    let payload_end = payload_start
        .checked_add(payload_len)
        .ok_or_else(|| DbError::internal("storage manifest: payload length overflow"))?;
    if payload_end + 4 != data.len() {
        return Err(DbError::internal(
            "storage manifest: payload length mismatch",
        ));
    }
    serde_json::from_slice(&data[payload_start..payload_end])
        .map_err(|error| DbError::internal(format!("storage manifest parse failed: {error}")))
}

fn read_storage_manifest_bytes(path: &Path) -> DbResult<Vec<u8>> {
    let file = File::open(path).map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to read {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to inspect {}: {error}",
            path.display()
        ))
    })?;
    if metadata.len() > MAX_STORAGE_MANIFEST_BYTES {
        return Err(DbError::program_limit(format!(
            "storage manifest: file size exceeds maximum {MAX_STORAGE_MANIFEST_BYTES} bytes"
        )));
    }
    let mut data = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAX_STORAGE_MANIFEST_BYTES.saturating_add(1));
    reader.read_to_end(&mut data).map_err(|error| {
        DbError::internal(format!(
            "storage manifest: failed to read {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > MAX_STORAGE_MANIFEST_BYTES {
        return Err(DbError::program_limit(format!(
            "storage manifest: file grew beyond maximum {MAX_STORAGE_MANIFEST_BYTES} bytes while reading"
        )));
    }
    Ok(data)
}

fn read_doctor_file_capped(
    path: &Path,
    max_bytes: u64,
    label: &str,
    report: &mut StorageDoctorReport,
) -> Option<Vec<u8>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            report.errors.push(format!(
                "failed to read {label} {}: {error}",
                path.display()
            ));
            return None;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            report.errors.push(format!(
                "failed to inspect {label} {}: {error}",
                path.display()
            ));
            return None;
        }
    };
    if metadata.len() > max_bytes {
        report.errors.push(format!(
            "{label} {} exceeds maximum {max_bytes} bytes",
            path.display()
        ));
        return None;
    }

    let mut data = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(max_bytes.saturating_add(1));
    if let Err(error) = reader.read_to_end(&mut data) {
        report.errors.push(format!(
            "failed to read {label} {}: {error}",
            path.display()
        ));
        return None;
    }
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > max_bytes {
        report.errors.push(format!(
            "{label} {} grew beyond maximum {max_bytes} bytes while reading",
            path.display()
        ));
        return None;
    }

    Some(data)
}

fn ensure_manifest_readable(manifest: &StorageManifest) -> DbResult<()> {
    if manifest.format_major < MIN_READABLE_STORAGE_FORMAT_MAJOR {
        return Err(DbError::internal(format!(
            "storage format v{}.{} is too old; export/import is required",
            manifest.format_major, manifest.format_minor
        )));
    }
    if manifest.format_major > MAX_READABLE_STORAGE_FORMAT_MAJOR {
        return Err(DbError::internal(format!(
            "storage format v{}.{} is newer than this binary supports",
            manifest.format_major, manifest.format_minor
        )));
    }
    Ok(())
}

fn inspect_tree(root: &Path, dir: &Path, report: &mut StorageDoctorReport) {
    let Ok(entries) = fs::read_dir(dir) else {
        report
            .errors
            .push(format!("cannot read directory {}", dir.display()));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            report
                .errors
                .push(format!("cannot stat {}", path.display()));
            continue;
        };
        if file_type.is_symlink() {
            report
                .errors
                .push(format!("refusing symlink in data-dir: {}", path.display()));
            continue;
        }
        if file_type.is_dir() {
            if is_upgrade_backup_dir(&path) {
                continue;
            }
            inspect_tree(root, &path, report);
        } else if file_type.is_file() {
            inspect_file(root, &path, report);
        }
    }
}

fn inspect_file(root: &Path, path: &Path, report: &mut StorageDoctorReport) {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if name == MANIFEST_FILE
        || has_extension_ignore_ascii_case(name, "tmp")
        || name.ends_with(CHECKSUM_SUFFIX)
    {
        return;
    }
    let rel_path = rel.to_string_lossy().replace('\\', "/");
    if is_lsm_artifact(rel, path) || is_experimental_path(rel) {
        report.experimental_files += 1;
        report.experimental_paths.push(rel_path.clone());
        report.warnings.push(format!(
            "experimental artifact has no compatibility promise: {}",
            rel.display()
        ));
        return;
    }
    if name.starts_with("wal_") && has_extension_ignore_ascii_case(name, "log") {
        report.stable_files += 1;
        report.wal_segments += 1;
        report.stable_paths.push(rel_path);
        inspect_magic_prefix(
            path,
            WAL_MAGIC,
            Some(LEGACY_WAL_MAGIC),
            "WAL segment",
            report,
        );
    } else if name == FPW_JOURNAL_FILENAME {
        report.stable_files += 1;
        report.fpw_journals += 1;
        report.stable_paths.push(rel_path);
        inspect_fpw_journal(path, report);
    } else if name == CHECKPOINT_MANIFEST_FILENAME && is_checkpoint_manifest_path(rel) {
        report.stable_files += 1;
        report.checkpoint_manifests += 1;
        report.stable_paths.push(rel_path);
        inspect_disk_checkpoint_manifest(path, report);
    } else if name == "base.snapshot" {
        report.stable_files += 1;
        report.storage_snapshots += 1;
        report.stable_paths.push(rel_path);
        inspect_snapshot(
            path,
            DATA_MAGIC,
            LEGACY_DATA_MAGIC,
            "storage snapshot",
            report,
        );
    } else if name == "catalog.snapshot" {
        report.stable_files += 1;
        report.catalog_snapshots += 1;
        report.stable_paths.push(rel_path);
        inspect_snapshot(
            path,
            CATALOG_MAGIC,
            LEGACY_CATALOG_MAGIC,
            "catalog snapshot",
            report,
        );
    } else if name.starts_with("data_") && has_extension_ignore_ascii_case(name, "db") {
        report.stable_files += 1;
        report.stable_paths.push(rel_path);
        match inspect_page_file(path, report) {
            PageFileKind::Heap => report.heap_page_files += 1,
            PageFileKind::Index => report.index_page_files += 1,
            PageFileKind::Mixed => report.mixed_page_files += 1,
            PageFileKind::PagedTable => report.paged_table_files += 1,
            PageFileKind::PagedSnapshot => report.storage_snapshots += 1,
            PageFileKind::Empty => report.empty_page_files += 1,
            PageFileKind::Corrupt => {}
        }
    }
}

fn inspect_disk_checkpoint_manifest(path: &Path, report: &mut StorageDoctorReport) {
    let Some(data) = read_doctor_file_capped(
        path,
        MAX_STORAGE_MANIFEST_BYTES,
        "checkpoint manifest",
        report,
    ) else {
        return;
    };
    let Some(json) = decode_checkpoint_manifest_json(path, &data, report) else {
        return;
    };
    let version = json.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(DISK_CHECKPOINT_MANIFEST_VERSION) {
        report.errors.push(format!(
            "checkpoint manifest {} has unsupported version {:?}",
            path.display(),
            version
        ));
    }
    let backend = json.get("backend").and_then(serde_json::Value::as_str);
    if backend != Some("disk") {
        report.errors.push(format!(
            "checkpoint manifest {} has invalid backend {:?}",
            path.display(),
            backend
        ));
    }
    if json
        .get("checkpoint_lsn")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        report.errors.push(format!(
            "checkpoint manifest {} is missing checkpoint_lsn",
            path.display()
        ));
    }
}

fn decode_checkpoint_manifest_json(
    path: &Path,
    data: &[u8],
    report: &mut StorageDoctorReport,
) -> Option<serde_json::Value> {
    if data.starts_with(CHECKPOINT_MANIFEST_MAGIC) {
        let min_len = CHECKPOINT_MANIFEST_MAGIC.len() + 8 + 4;
        if data.len() < min_len {
            report.errors.push(format!(
                "checkpoint manifest {} is truncated",
                path.display()
            ));
            return None;
        }
        let checksum_offset = data.len() - 4;
        let Ok(stored) = read_fixed::<4>(data, checksum_offset, "checkpoint manifest checksum")
            .map(u32::from_le_bytes)
        else {
            report.errors.push(format!(
                "checkpoint manifest {} checksum is truncated",
                path.display()
            ));
            return None;
        };
        let computed = compute_crc32c(&data[..checksum_offset]);
        if stored != computed {
            report.errors.push(format!(
                "checkpoint manifest {} checksum mismatch",
                path.display()
            ));
            return None;
        }
        let Ok(payload_len_bytes) = read_fixed::<8>(
            data,
            CHECKPOINT_MANIFEST_MAGIC.len(),
            "checkpoint manifest length",
        ) else {
            report.errors.push(format!(
                "checkpoint manifest {} length is truncated",
                path.display()
            ));
            return None;
        };
        let Ok(payload_len) = usize::try_from(u64::from_le_bytes(payload_len_bytes)) else {
            report.errors.push(format!(
                "checkpoint manifest {} payload length overflows usize",
                path.display()
            ));
            return None;
        };
        let payload_start = CHECKPOINT_MANIFEST_MAGIC.len() + 8;
        let Some(payload_end) = payload_start.checked_add(payload_len) else {
            report.errors.push(format!(
                "checkpoint manifest {} payload length overflow",
                path.display()
            ));
            return None;
        };
        if payload_end + 4 != data.len() {
            report.errors.push(format!(
                "checkpoint manifest {} payload length mismatch",
                path.display()
            ));
            return None;
        }
        return serde_json::from_slice::<serde_json::Value>(&data[payload_start..payload_end])
            .map_err(|error| {
                report.errors.push(format!(
                    "checkpoint manifest {} framed JSON is invalid: {error}",
                    path.display()
                ));
            })
            .ok();
    }

    serde_json::from_slice::<serde_json::Value>(data)
        .map_err(|error| {
            report.errors.push(format!(
                "checkpoint manifest {} legacy JSON is invalid: {error}",
                path.display()
            ));
        })
        .ok()
}

fn inspect_fpw_journal(path: &Path, report: &mut StorageDoctorReport) {
    let Some(data) = read_doctor_file_capped(
        path,
        u64::try_from(FPW_JOURNAL_RECORD_BYTES).unwrap_or(u64::MAX),
        "FPW journal",
        report,
    ) else {
        return;
    };
    if data.is_empty() {
        report.warnings.push(format!(
            "FPW journal {} is empty and will be ignored during recovery",
            path.display()
        ));
        return;
    }
    if data.len() != FPW_JOURNAL_RECORD_BYTES {
        report.errors.push(format!(
            "FPW journal {} has invalid size {}, expected {}",
            path.display(),
            data.len(),
            FPW_JOURNAL_RECORD_BYTES
        ));
        return;
    }
    if &data[..FPW_JOURNAL_MAGIC.len()] != FPW_JOURNAL_MAGIC {
        report
            .errors
            .push(format!("FPW journal {} has invalid magic", path.display()));
        return;
    }
    let checksum_offset = data.len() - 4;
    let Ok(stored) =
        read_fixed::<4>(&data, checksum_offset, "FPW journal checksum").map(u32::from_le_bytes)
    else {
        report.errors.push(format!(
            "FPW journal {} checksum is truncated",
            path.display()
        ));
        return;
    };
    let computed = compute_crc32c(&data[..checksum_offset]);
    if stored != computed {
        report
            .errors
            .push(format!("FPW journal {} checksum mismatch", path.display()));
    }
}

fn inspect_magic_prefix(
    path: &Path,
    current_magic: &[u8],
    legacy_magic: Option<&[u8]>,
    label: &str,
    report: &mut StorageDoctorReport,
) {
    let mut prefix = vec![0u8; current_magic.len()];
    match File::open(path).and_then(|mut file| file.read_exact(&mut prefix)) {
        Ok(()) => {
            if prefix != current_magic && legacy_magic.map_or(true, |legacy| prefix != legacy) {
                report
                    .errors
                    .push(format!("{label} {} has invalid magic", path.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            report
                .errors
                .push(format!("{label} {} is truncated", path.display()));
        }
        Err(error) => report.errors.push(format!(
            "failed to inspect {label} {}: {error}",
            path.display()
        )),
    }
}

fn inspect_snapshot(
    path: &Path,
    current_magic: &[u8],
    legacy_magic: &[u8],
    label: &str,
    report: &mut StorageDoctorReport,
) {
    let Ok(data) = fs::read(path) else {
        report
            .errors
            .push(format!("failed to read {label} {}", path.display()));
        return;
    };
    if data.len() < current_magic.len() + 4 {
        report
            .errors
            .push(format!("{label} {} is too small", path.display()));
        return;
    }
    if &data[..current_magic.len()] != current_magic && &data[..legacy_magic.len()] != legacy_magic
    {
        report
            .errors
            .push(format!("{label} {} has invalid magic", path.display()));
        return;
    }
    let checksum_offset = data.len() - 4;
    let Ok(stored) = read_fixed::<4>(&data, checksum_offset, "checksum").map(u32::from_le_bytes)
    else {
        report
            .errors
            .push(format!("{label} {} checksum is truncated", path.display()));
        return;
    };
    let computed = compute_crc32c(&data[..checksum_offset]);
    if stored != computed && stored != compute_legacy_fnv1a(&data[..checksum_offset]) {
        report
            .errors
            .push(format!("{label} {} checksum mismatch", path.display()));
    }
}

fn inspect_page_file(path: &Path, report: &mut StorageDoctorReport) -> PageFileKind {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            report.errors.push(format!(
                "failed to read page file {}: {error}",
                path.display()
            ));
            return PageFileKind::Corrupt;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            report.errors.push(format!(
                "failed to inspect page file {}: {error}",
                path.display()
            ));
            return PageFileKind::Corrupt;
        }
    };
    let page_size_u64 = u64::try_from(PAGE_SIZE).unwrap_or(u64::MAX);
    if metadata.len() % page_size_u64 != 0 {
        report.errors.push(format!(
            "page file {} size {} is not a multiple of page size {}",
            path.display(),
            metadata.len(),
            PAGE_SIZE
        ));
        return PageFileKind::Corrupt;
    }
    let page_count = metadata.len() / page_size_u64;
    let relation_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(parse_relation_id_from_page_file);
    let checksums = read_page_checksum_sidecar(path, page_count, report);
    if relation_id.is_some_and(is_paged_snapshot_relation_id) {
        inspect_streamed_page_checksums(
            path,
            &mut file,
            checksums.as_deref(),
            0,
            page_count,
            report,
        );
        return PageFileKind::PagedSnapshot;
    }

    let mut has_heap_pages = false;
    let mut has_index_pages = false;
    let mut has_non_empty_pages = false;
    let mut has_paged_table_header = false;
    let mut has_paged_snapshot_header = false;
    let mut page = vec![0; PAGE_SIZE];
    let mut inspected_pages = 0;
    for page_no in 0..page_count {
        if let Err(error) = file.read_exact(&mut page) {
            report.errors.push(format!(
                "failed to read page file {} page {}: {error}",
                path.display(),
                page_no
            ));
            return PageFileKind::Corrupt;
        }
        inspected_pages = page_no.saturating_add(1);
        inspect_single_page_checksum(path, &page, page_no, checksums.as_deref(), report);
        if page.iter().all(|byte| *byte == 0) {
            continue;
        }
        has_non_empty_pages = true;
        let magic = &page[..8];
        if magic == PAGED_TABLE_MAGIC {
            has_paged_table_header = true;
            break;
        } else if magic == PAGED_SNAPSHOT_HEADER_MAGIC || magic == PAGED_SNAPSHOT_PUBLISHED_MAGIC {
            has_paged_snapshot_header = true;
            break;
        } else if magic == HEAP_PAGE_MAGIC {
            has_heap_pages = true;
        } else if magic == BTREE_META_MAGIC
            || magic == BTREE_PAGE_MAGIC
            || magic == VAR_BTREE_META_MAGIC
            || magic == VAR_BTREE_LEAF_MAGIC
            || magic == VAR_BTREE_INTERNAL_MAGIC
        {
            has_index_pages = true;
        } else {
            report.errors.push(format!(
                "page file {} page {} has unknown magic",
                path.display(),
                page_no
            ));
            return PageFileKind::Corrupt;
        }
    }
    inspect_streamed_page_checksums(
        path,
        &mut file,
        checksums.as_deref(),
        inspected_pages,
        page_count,
        report,
    );
    if has_paged_table_header {
        return PageFileKind::PagedTable;
    }
    if has_paged_snapshot_header {
        return PageFileKind::PagedSnapshot;
    }
    match (has_non_empty_pages, has_heap_pages, has_index_pages) {
        (false, _, _) => PageFileKind::Empty,
        (true, true, false) => PageFileKind::Heap,
        (true, false, true) => PageFileKind::Index,
        (true, true, true) => PageFileKind::Mixed,
        (true, false, false) => PageFileKind::Corrupt,
    }
}

fn parse_relation_id_from_page_file(file_name: &str) -> Option<u64> {
    file_name
        .strip_prefix("data_")?
        .strip_suffix(".db")?
        .parse()
        .ok()
}

fn is_paged_snapshot_relation_id(relation_id: u64) -> bool {
    relation_id == PAGED_SNAPSHOT_HEADER_RELATION_ID
        || relation_id == PAGED_SNAPSHOT_PUBLISHED_RELATION_ID
        || PAGED_SNAPSHOT_SLOT_RELATION_IDS.contains(&relation_id)
}

fn read_page_checksum_sidecar(
    path: &Path,
    page_count: u64,
    report: &mut StorageDoctorReport,
) -> Option<Vec<u8>> {
    let checksum_path = PathBuf::from(format!("{}{}", path.display(), CHECKSUM_SUFFIX));
    let file = match File::open(&checksum_path) {
        Ok(file) => file,
        Err(_) => {
            report.warnings.push(format!(
                "page checksum sidecar missing: {}",
                checksum_path.display()
            ));
            return None;
        }
    };
    let Some(expected_bytes) = page_count.checked_mul(4) else {
        report.errors.push(format!(
            "page checksum sidecar expected size overflows for {}",
            checksum_path.display()
        ));
        return None;
    };
    if expected_bytes > MAX_DOCTOR_CHECKSUM_SIDECAR_BYTES {
        report.warnings.push(format!(
            "page checksum sidecar {} is too large to inspect in memory ({} bytes)",
            checksum_path.display(),
            expected_bytes
        ));
        return None;
    }
    let mut checksums = Vec::with_capacity(usize::try_from(expected_bytes).unwrap_or(0));
    let mut reader = file.take(expected_bytes);
    if let Err(error) = reader.read_to_end(&mut checksums) {
        report.warnings.push(format!(
            "failed to read page checksum sidecar {}: {error}",
            checksum_path.display()
        ));
        return None;
    }
    Some(checksums)
}

fn inspect_streamed_page_checksums(
    path: &Path,
    file: &mut File,
    checksums: Option<&[u8]>,
    start_page: u64,
    page_count: u64,
    report: &mut StorageDoctorReport,
) {
    let mut page = vec![0; PAGE_SIZE];
    for page_no in start_page..page_count {
        if let Err(error) = file.read_exact(&mut page) {
            report.errors.push(format!(
                "failed to read page file {} page {}: {error}",
                path.display(),
                page_no
            ));
            return;
        }
        inspect_single_page_checksum(path, &page, page_no, checksums, report);
    }
}

fn inspect_single_page_checksum(
    path: &Path,
    page: &[u8],
    page_no: u64,
    checksums: Option<&[u8]>,
    report: &mut StorageDoctorReport,
) {
    let Some(checksums) = checksums else {
        return;
    };
    let Some(offset) = usize::try_from(page_no)
        .ok()
        .and_then(|page_no| page_no.checked_mul(4))
    else {
        report.errors.push(format!(
            "page checksum sidecar offset overflows for {} page {}",
            path.display(),
            page_no
        ));
        return;
    };
    if offset + 4 > checksums.len() {
        report.errors.push(format!(
            "page checksum sidecar is truncated for {} page {}",
            path.display(),
            page_no
        ));
        return;
    }
    let mut stored = [0u8; 4];
    stored.copy_from_slice(&checksums[offset..offset + 4]);
    if u32::from_le_bytes(stored) != compute_crc32c(page) {
        report.errors.push(format!(
            "page checksum mismatch in {} page {}",
            path.display(),
            page_no
        ));
    }
}

fn data_dir_has_stable_artifacts(data_dir: &Path) -> DbResult<bool> {
    let report = doctor_data_dir_without_manifest_error(data_dir)?;
    Ok(report.stable_files > 0)
}

fn doctor_data_dir_without_manifest_error(data_dir: &Path) -> DbResult<StorageDoctorReport> {
    if !data_dir.exists() {
        return Ok(StorageDoctorReport {
            data_dir: data_dir.to_path_buf(),
            format_major: None,
            format_minor: None,
            manifest_present: false,
            stable_files: 0,
            wal_segments: 0,
            catalog_snapshots: 0,
            storage_snapshots: 0,
            paged_table_files: 0,
            fpw_journals: 0,
            checkpoint_manifests: 0,
            heap_page_files: 0,
            index_page_files: 0,
            mixed_page_files: 0,
            empty_page_files: 0,
            experimental_files: 0,
            stable_paths: Vec::new(),
            experimental_paths: Vec::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
            upgrade_possible: true,
        });
    }
    let mut report = StorageDoctorReport {
        data_dir: data_dir.to_path_buf(),
        format_major: None,
        format_minor: None,
        manifest_present: false,
        stable_files: 0,
        wal_segments: 0,
        catalog_snapshots: 0,
        storage_snapshots: 0,
        paged_table_files: 0,
        fpw_journals: 0,
        checkpoint_manifests: 0,
        heap_page_files: 0,
        index_page_files: 0,
        mixed_page_files: 0,
        empty_page_files: 0,
        experimental_files: 0,
        stable_paths: Vec::new(),
        experimental_paths: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
        upgrade_possible: false,
    };
    inspect_tree(data_dir, data_dir, &mut report);
    Ok(report)
}

fn upgrade_is_possible(report: &StorageDoctorReport) -> bool {
    if !report.errors.is_empty() {
        return false;
    }
    match report.format_major {
        None => true,
        Some(STORAGE_FORMAT_MAJOR) => true,
        Some(major) if major < STORAGE_FORMAT_MAJOR => true,
        Some(_) => false,
    }
}

fn backup_data_dir(data_dir: &Path) -> DbResult<PathBuf> {
    let parent = data_dir.parent().unwrap_or_else(|| Path::new("."));
    let name = data_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aiondb-data");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| DbError::internal(format!("system clock before UNIX_EPOCH: {error}")))?
        .as_secs();
    let backup = unique_backup_path(parent, name, timestamp)?;
    copy_dir_recursive(data_dir, &backup)?;
    Ok(backup)
}

fn unique_backup_path(parent: &Path, name: &str, timestamp: u64) -> DbResult<PathBuf> {
    for attempt in 0..1000u16 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let backup = parent.join(format!(
            "{name}.backup-before-storage-v1-{timestamp}{suffix}"
        ));
        if !backup.exists() {
            return Ok(backup);
        }
    }
    Err(DbError::internal(format!(
        "failed to allocate unique storage upgrade backup path for {}",
        parent.join(name).display()
    )))
}

const MAX_UPGRADE_BACKUP_COPY_DEPTH: usize = 256;

fn copy_dir_recursive(source: &Path, target: &Path) -> DbResult<()> {
    copy_dir_recursive_at_depth(source, target, 0)
}

fn copy_dir_recursive_at_depth(source: &Path, target: &Path, depth: usize) -> DbResult<()> {
    if depth >= MAX_UPGRADE_BACKUP_COPY_DEPTH {
        return Err(DbError::program_limit(format!(
            "upgrade backup directory depth exceeds limit {MAX_UPGRADE_BACKUP_COPY_DEPTH}"
        )));
    }
    fs::create_dir_all(target).map_err(|error| {
        DbError::internal(format!(
            "failed to create upgrade backup {}: {error}",
            target.display()
        ))
    })?;
    for entry in fs::read_dir(source).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate upgrade backup source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!("failed to read upgrade backup entry: {error}"))
        })?;
        let source_path = entry.path();
        if is_upgrade_backup_dir(&source_path) {
            continue;
        }
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat upgrade backup source {}: {error}",
                source_path.display()
            ))
        })?;
        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing symlink during upgrade backup: {}",
                source_path.display()
            )));
        }
        if file_type.is_dir() {
            copy_dir_recursive_at_depth(&source_path, &target_path, depth + 1)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                DbError::internal(format!(
                    "failed to copy upgrade backup file {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                ))
            })?;
        }
    }
    Ok(())
}

fn is_upgrade_backup_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".backup-before-storage-v1-"))
}

fn is_lsm_artifact(rel: &Path, path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if has_extension_ignore_ascii_case(name, "sst") || name.ends_with(".sst.json") {
        return true;
    }
    let text = rel.to_string_lossy().to_ascii_lowercase();
    if text.contains("/levels/") || text.starts_with("levels/") {
        return true;
    }
    name == CHECKPOINT_MANIFEST_FILENAME
        && read_storage_manifest_bytes(path)
            .map(|content| {
                let content = String::from_utf8_lossy(&content);
                content.contains("\"backend\"") && content.contains("\"lsm\"")
            })
            .unwrap_or(false)
}

fn has_extension_ignore_ascii_case(name: &str, expected: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(expected))
}

fn is_checkpoint_manifest_path(rel: &Path) -> bool {
    let text = rel.to_string_lossy().replace('\\', "/");
    text == "checkpoints/manifest.json" || text.ends_with("/checkpoints/manifest.json")
}

fn is_experimental_path(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("hnsw")
        || text.contains("vector")
        || text.contains("graph")
        || text.contains("distributed")
        || text.contains("raft")
        || text.contains("ha")
        || text.contains("lsm")
}

fn read_fixed<const N: usize>(data: &[u8], offset: usize, field: &str) -> DbResult<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| DbError::internal(format!("storage compatibility: {field} overflow")))?;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| DbError::internal(format!("storage compatibility: {field} truncated")))?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::checksum::compute_crc32c;
    use aiondb_wal::{segment, WalConfig, WalLsnMode, WalRecord, WalWriter};

    fn test_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "aiondb-storage-compat-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    struct LocalWalAuthGuard;

    impl LocalWalAuthGuard {
        fn enable() -> Self {
            std::env::set_var(
                "AIONDB_WAL_LOCAL_HMAC_KEY",
                "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
            );
            Self
        }
    }

    impl Drop for LocalWalAuthGuard {
        fn drop(&mut self) {
            std::env::remove_var("AIONDB_WAL_LOCAL_HMAC_KEY");
        }
    }

    #[test]
    fn empty_dir_gets_manifest_for_open() {
        let dir = test_dir("empty-dir-gets-manifest");
        ensure_storage_contract_for_open(&dir, StorageBackendKind::Durable).unwrap();
        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.format_major, Some(STORAGE_FORMAT_MAJOR));
    }

    #[test]
    fn existing_stable_files_without_manifest_require_upgrade() {
        let dir = test_dir("requires-upgrade");
        let wal = dir.join("wal");
        fs::create_dir_all(&wal).unwrap();
        let mut bytes = vec![0u8; 32];
        bytes[..8].copy_from_slice(WAL_MAGIC);
        fs::write(wal.join("wal_000000000001.log"), bytes).unwrap();
        let err = ensure_storage_contract_for_open(&dir, StorageBackendKind::Durable)
            .expect_err("existing data-dir without manifest must be refused");
        assert!(err.to_string().contains("aiondb upgrade"));
    }

    #[test]
    fn doctor_reports_stable_storage_categories() {
        let dir = test_dir("stable-categories");
        let wal = dir.join("wal");
        fs::create_dir_all(&wal).unwrap();

        let mut wal_bytes = vec![0u8; 32];
        wal_bytes[..8].copy_from_slice(WAL_MAGIC);
        fs::write(wal.join("wal_000000000001.log"), wal_bytes).unwrap();

        let mut heap_page = vec![0u8; PAGE_SIZE];
        heap_page[..8].copy_from_slice(HEAP_PAGE_MAGIC);
        fs::write(dir.join("data_000001.db"), &heap_page).unwrap();
        fs::write(
            dir.join("data_000001.db.csum"),
            compute_crc32c(&heap_page).to_le_bytes(),
        )
        .unwrap();

        let mut index_page = vec![0u8; PAGE_SIZE];
        index_page[..8].copy_from_slice(BTREE_PAGE_MAGIC);
        fs::write(dir.join("data_000002.db"), &index_page).unwrap();
        fs::write(
            dir.join("data_000002.db.csum"),
            compute_crc32c(&index_page).to_le_bytes(),
        )
        .unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.wal_segments, 1);
        assert_eq!(report.heap_page_files, 1);
        assert_eq!(report.index_page_files, 1);
        assert_eq!(report.stable_files, 3);
        assert!(report
            .stable_paths
            .iter()
            .any(|path| path == "wal/wal_000000000001.log"));
    }

    #[test]
    fn doctor_accepts_paged_table_files_with_raw_data_pages() {
        let dir = test_dir("paged-table-file");
        fs::create_dir_all(&dir).unwrap();

        let mut first_page = vec![0u8; PAGE_SIZE];
        first_page[..8].copy_from_slice(PAGED_TABLE_MAGIC);
        let mut data_page = vec![0u8; PAGE_SIZE];
        data_page[..16].copy_from_slice(b"raw row payload!");
        let mut file_bytes = first_page.clone();
        file_bytes.extend_from_slice(&data_page);
        fs::write(dir.join("data_000003.db"), &file_bytes).unwrap();

        let mut checksums = Vec::new();
        checksums.extend_from_slice(&compute_crc32c(&first_page).to_le_bytes());
        checksums.extend_from_slice(&compute_crc32c(&data_page).to_le_bytes());
        fs::write(dir.join("data_000003.db.csum"), checksums).unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.paged_table_files, 1);
        assert_eq!(report.stable_files, 1);
    }

    #[test]
    fn doctor_accepts_paged_snapshot_slot_files_with_raw_payload() {
        let dir = test_dir("paged-snapshot-slot");
        fs::create_dir_all(&dir).unwrap();

        let mut slot_page = vec![0u8; PAGE_SIZE];
        slot_page[..8].copy_from_slice(&32u64.to_le_bytes());
        slot_page[8..24].copy_from_slice(b"snapshot payload");
        let file_name = format!("data_{}.db", PAGED_SNAPSHOT_SLOT_RELATION_IDS[0]);
        fs::write(dir.join(&file_name), &slot_page).unwrap();
        fs::write(
            dir.join(format!("{file_name}.csum")),
            compute_crc32c(&slot_page).to_le_bytes(),
        )
        .unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.storage_snapshots, 1);
        assert_eq!(report.stable_files, 1);
    }

    #[test]
    fn doctor_reports_and_validates_fpw_journal() {
        let dir = test_dir("fpw-journal");
        fs::create_dir_all(&dir).unwrap();

        let mut record = Vec::new();
        record.extend_from_slice(FPW_JOURNAL_MAGIC);
        record.extend_from_slice(&7u64.to_le_bytes());
        record.extend_from_slice(&2u64.to_le_bytes());
        record.extend_from_slice(&vec![0x5au8; PAGE_SIZE]);
        let checksum = compute_crc32c(&record);
        record.extend_from_slice(&checksum.to_le_bytes());
        fs::write(dir.join(FPW_JOURNAL_FILENAME), record).unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.fpw_journals, 1);
        assert_eq!(report.stable_files, 1);
    }

    #[test]
    fn corrupt_fpw_journal_blocks_upgrade() {
        let dir = test_dir("fpw-journal-corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FPW_JOURNAL_FILENAME), b"AIONFPW1bad").unwrap();

        let report = doctor_data_dir(&dir);
        assert!(!report.ok(), "{report:?}");
        assert!(!report.upgrade_possible, "{report:?}");
    }

    #[test]
    fn doctor_marks_lsm_artifacts_experimental() {
        let dir = test_dir("lsm-experimental");
        let level_dir = dir.join("levels").join("level-0");
        fs::create_dir_all(&level_dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            br#"{"version":1,"backend":"lsm"}"#,
        )
        .unwrap();
        fs::write(level_dir.join("000001.sst"), b"AIONSST1").unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.stable_files, 0);
        assert_eq!(report.experimental_files, 2);
        assert!(report.upgrade_possible, "{report:?}");
    }

    #[test]
    fn doctor_validates_disk_checkpoint_manifest() {
        let dir = test_dir("checkpoint-manifest");
        let checkpoint_dir = dir.join("checkpoints");
        fs::create_dir_all(&checkpoint_dir).unwrap();
        fs::write(
            checkpoint_dir.join(CHECKPOINT_MANIFEST_FILENAME),
            br#"{"version":1,"backend":"disk","checkpoint_lsn":42}"#,
        )
        .unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.checkpoint_manifests, 1);
        assert_eq!(report.stable_files, 1);
    }

    #[test]
    fn doctor_validates_framed_disk_checkpoint_manifest() {
        let dir = test_dir("checkpoint-manifest-framed");
        let checkpoint_dir = dir.join("checkpoints");
        fs::create_dir_all(&checkpoint_dir).unwrap();
        let payload = br#"{"version":1,"backend":"disk","checkpoint_lsn":77}"#;
        let mut framed = Vec::new();
        framed.extend_from_slice(CHECKPOINT_MANIFEST_MAGIC);
        framed.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        framed.extend_from_slice(payload);
        let checksum = compute_crc32c(&framed);
        framed.extend_from_slice(&checksum.to_le_bytes());
        fs::write(checkpoint_dir.join(CHECKPOINT_MANIFEST_FILENAME), framed).unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.checkpoint_manifests, 1);
        assert_eq!(report.stable_files, 1);
    }

    #[test]
    fn corrupt_disk_checkpoint_manifest_blocks_upgrade() {
        let dir = test_dir("checkpoint-manifest-corrupt");
        let checkpoint_dir = dir.join("checkpoints");
        fs::create_dir_all(&checkpoint_dir).unwrap();
        fs::write(
            checkpoint_dir.join(CHECKPOINT_MANIFEST_FILENAME),
            br#"{"version":2,"backend":"disk"}"#,
        )
        .unwrap();

        let report = doctor_data_dir(&dir);
        assert!(!report.ok(), "{report:?}");
        assert!(!report.upgrade_possible, "{report:?}");
    }

    #[test]
    fn upgrade_creates_backup_and_manifest() {
        let dir = test_dir("upgrade");
        fs::create_dir_all(&dir).unwrap();
        let backup = upgrade_data_dir(&dir).unwrap();
        assert!(manifest_path(&dir).is_file());
        assert!(backup.is_dir());
        assert!(doctor_data_dir(&dir).ok());
    }

    #[test]
    fn corrupt_manifest_refuses_doctor_and_upgrade() {
        let dir = test_dir("corrupt-manifest");
        fs::create_dir_all(&dir).unwrap();
        fs::write(manifest_path(&dir), b"AIONFMT1bad").unwrap();

        let report = doctor_data_dir(&dir);
        assert!(!report.ok(), "{report:?}");
        assert!(!report.upgrade_possible, "{report:?}");
        assert!(upgrade_data_dir(&dir).is_err());
    }

    #[test]
    fn experimental_artifacts_warn_without_blocking_stable_upgrade() {
        let dir = test_dir("experimental-artifacts");
        let graph_dir = dir.join("graph");
        fs::create_dir_all(&graph_dir).unwrap();
        fs::write(graph_dir.join("labels.bin"), b"experimental").unwrap();

        let report = doctor_data_dir(&dir);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.experimental_files, 1);
        assert!(report.upgrade_possible, "{report:?}");
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("experimental artifact")));
    }

    #[test]
    fn doctor_rejects_authenticated_wal_tampering_with_recomputed_crc() {
        let _auth = LocalWalAuthGuard::enable();
        let dir = test_dir("doctor-authenticated-wal-tamper");
        let wal_dir = dir.join("wal");
        let config = WalConfig {
            dir: wal_dir.clone(),
            segment_max_bytes: 16 * 1024 * 1024,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: WalLsnMode::Logical,
        };
        let mut writer = WalWriter::open(config).unwrap();
        writer
            .append(&WalRecord::Checkpoint {
                last_committed_lsn: aiondb_wal::Lsn::ZERO,
            })
            .unwrap();
        writer.flush_durable().unwrap();
        drop(writer);

        let seg_id = segment::SegmentId::new(1);
        let path = wal_dir.join(seg_id.filename());
        let mut bytes = fs::read(&path).unwrap();
        let entry_offset = segment::entry_data_offset(&bytes).unwrap();
        let (_entry, consumed) = aiondb_wal::codec::decode_entry(&bytes[entry_offset..]).unwrap();
        let entry_end = entry_offset + consumed;
        let payload_last_byte = entry_end - 5;
        bytes[payload_last_byte] ^= 0x01;
        let checksum = compute_crc32c(&bytes[entry_offset + 4..entry_end - 4]);
        bytes[entry_end - 4..entry_end].copy_from_slice(&checksum.to_le_bytes());
        fs::write(&path, bytes).unwrap();

        let report = doctor_data_dir(&dir);
        assert!(!report.ok(), "{report:?}");
        assert!(report
            .errors
            .iter()
            .any(|error| { error.contains("WAL local integrity verification failed") }));
    }
}
