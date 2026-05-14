use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::WalLsnMode;
use aiondb_core::{DbError, DbResult};
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
thread_local! {
    static FAIL_DIR_SYNC_COUNTDOWN: Cell<Option<usize>> = const { Cell::new(None) };
    static TEST_LOCAL_HMAC_KEY_OVERRIDE: RefCell<TestLocalHmacKeyOverride> =
        const { RefCell::new(TestLocalHmacKeyOverride::Unset) };
}

#[cfg(test)]
static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
enum TestLocalHmacKeyOverride {
    Unset,
    Set(Option<Vec<u8>>),
}

/// WAL segment file magic bytes used for explicit stable format identification.
const SEGMENT_MAGIC: &[u8; 8] = b"AIONWAL1";
/// Historical v0.1 WAL segment magic accepted for explicit upgrades.
const LEGACY_SEGMENT_MAGIC: &[u8; 8] = b"AIONWAL\0";
const SEGMENT_FORMAT_VERSION_V1: u8 = 1;
const SEGMENT_FORMAT_VERSION_V2: u8 = 2;
/// Current WAL segment format version.
const SEGMENT_FORMAT_VERSION: u8 = 3;
/// Number of bytes in the v1 segment header (`magic + version`).
const SEGMENT_HEADER_SIZE_V1: usize = SEGMENT_MAGIC.len() + 1;
/// Number of bytes in the v2 segment header (`magic + version + lsn_mode`).
const SEGMENT_HEADER_SIZE_V2: usize = SEGMENT_MAGIC.len() + 2;
/// Number of bytes in the current segment header (`magic + version + lsn_mode + system_id + timeline`).
const SEGMENT_HEADER_SIZE: usize = SEGMENT_MAGIC.len() + 2 + 8 + 4;
const WAL_ARCHIVE_DIR_ENV: &str = "AIONDB_WAL_ARCHIVE_DIR";
const WAL_RESTORE_FROM_ARCHIVE_ENV: &str = "AIONDB_WAL_RESTORE_FROM_ARCHIVE";
const WAL_ARCHIVE_HMAC_KEY_ENV: &str = "AIONDB_WAL_ARCHIVE_HMAC_KEY";
const WAL_ARCHIVE_HMAC_SUFFIX: &str = ".hmac";
const WAL_LOCAL_HMAC_KEY_ENV: &str = "AIONDB_WAL_LOCAL_HMAC_KEY";
const WAL_LOCAL_HMAC_SUFFIX: &str = ".auth";
const WAL_LOCAL_HMAC_MAGIC: &[u8; 8] = b"AIONAUTH";
const WAL_LOCAL_HMAC_VERSION: u8 = 1;
const WAL_HMAC_TAG_BYTES: usize = 32;
const WAL_LOCAL_HMAC_SIZE_BYTES: usize = WAL_LOCAL_HMAC_MAGIC.len() + 1 + 8 + WAL_HMAC_TAG_BYTES;
const MAX_CLUSTER_IDENTITY_FILE_BYTES: u64 = 64;
const WAL_RECYCLED_SEGMENT_PREFIX: &str = ".wal_recycled_";
const WAL_RECYCLED_SEGMENT_SUFFIX: &str = ".log";
/// Hard safety ceiling for WAL segment reads performed during recovery/scan.
///
/// This caps single-buffer allocations when reading potentially corrupted or
/// attacker-controlled files on disk.
pub const WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const WAL_HMAC_STREAM_BUFFER_BYTES: usize = 64 * 1024;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalWalAuthVerification {
    pub trusted_len: u64,
    pub truncated_unauthenticated_tail: bool,
}

pub(crate) struct LocalWalAuthState {
    mac: HmacSha256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SegmentLsnMode {
    Logical = 1,
    ByteOffset = 2,
}

impl SegmentLsnMode {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Logical),
            2 => Some(Self::ByteOffset),
            _ => None,
        }
    }
}

impl From<WalLsnMode> for SegmentLsnMode {
    fn from(value: WalLsnMode) -> Self {
        match value {
            WalLsnMode::Logical => Self::Logical,
            WalLsnMode::ByteOffset => Self::ByteOffset,
        }
    }
}

impl From<SegmentLsnMode> for WalLsnMode {
    fn from(value: SegmentLsnMode) -> Self {
        match value {
            SegmentLsnMode::Logical => Self::Logical,
            SegmentLsnMode::ByteOffset => Self::ByteOffset,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SegmentHeaderInfo {
    pub(crate) format_version: Option<u8>,
    pub(crate) entry_offset: usize,
    pub(crate) lsn_mode: Option<SegmentLsnMode>,
    pub(crate) system_identifier: Option<u64>,
    pub(crate) timeline_id: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentHeaderSummary {
    pub format_version: Option<u8>,
    pub entry_offset: usize,
    pub lsn_mode: Option<WalLsnMode>,
    pub system_identifier: Option<u64>,
    pub timeline_id: Option<u32>,
}

impl From<SegmentHeaderInfo> for SegmentHeaderSummary {
    fn from(value: SegmentHeaderInfo) -> Self {
        Self {
            format_version: value.format_version,
            entry_offset: value.entry_offset,
            lsn_mode: value.lsn_mode.map(Into::into),
            system_identifier: value.system_identifier,
            timeline_id: value.timeline_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SegmentClusterIdentity {
    pub(crate) system_identifier: Option<u64>,
    pub(crate) timeline_id: Option<u32>,
}

fn segment_header_bytes(
    lsn_mode: SegmentLsnMode,
    identity: SegmentClusterIdentity,
) -> [u8; SEGMENT_HEADER_SIZE] {
    let mut header = [0u8; SEGMENT_HEADER_SIZE];
    header[..SEGMENT_MAGIC.len()].copy_from_slice(SEGMENT_MAGIC);
    header[SEGMENT_MAGIC.len()] = SEGMENT_FORMAT_VERSION;
    header[SEGMENT_MAGIC.len() + 1] = lsn_mode as u8;
    header[SEGMENT_MAGIC.len() + 2..SEGMENT_MAGIC.len() + 10]
        .copy_from_slice(&identity.system_identifier.unwrap_or(0).to_le_bytes());
    header[SEGMENT_MAGIC.len() + 10..SEGMENT_MAGIC.len() + 14]
        .copy_from_slice(&identity.timeline_id.unwrap_or(0).to_le_bytes());
    header
}

fn has_segment_magic(data: &[u8]) -> bool {
    data.len() >= SEGMENT_MAGIC.len()
        && (&data[..SEGMENT_MAGIC.len()] == SEGMENT_MAGIC
            || &data[..LEGACY_SEGMENT_MAGIC.len()] == LEGACY_SEGMENT_MAGIC)
}

pub(crate) fn resolve_cluster_identity_from_wal_dir(wal_dir: &Path) -> SegmentClusterIdentity {
    let mut current = Some(wal_dir);
    while let Some(dir) = current {
        let replication_dir = dir.join("replication");
        let system_identifier = read_cluster_identity_file(&replication_dir.join("system_id"))
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|value| *value > 0);
        let timeline_id = read_cluster_identity_file(&replication_dir.join("timeline"))
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .filter(|value| *value > 0);
        if system_identifier.is_some() || timeline_id.is_some() {
            return SegmentClusterIdentity {
                system_identifier,
                timeline_id,
            };
        }
        current = dir.parent();
    }
    SegmentClusterIdentity::default()
}

fn read_cluster_identity_file(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_CLUSTER_IDENTITY_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cluster identity file {} exceeds maximum {} bytes",
                path.display(),
                MAX_CLUSTER_IDENTITY_FILE_BYTES
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAX_CLUSTER_IDENTITY_FILE_BYTES.saturating_add(1));
    reader.read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CLUSTER_IDENTITY_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cluster identity file {} grew beyond maximum {} bytes",
                path.display(),
                MAX_CLUSTER_IDENTITY_FILE_BYTES
            ),
        ));
    }

    String::from_utf8(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cluster identity file {} is not UTF-8: {error}",
                path.display()
            ),
        )
    })
}

#[cfg(test)]
fn should_fail_dir_sync() -> bool {
    FAIL_DIR_SYNC_COUNTDOWN.with(|countdown| match countdown.get() {
        Some(0) => {
            countdown.set(None);
            true
        }
        Some(remaining) => {
            countdown.set(Some(remaining - 1));
            false
        }
        None => false,
    })
}

#[cfg(test)]
pub fn set_test_local_hmac_key_override(value: Option<Vec<u8>>) {
    TEST_LOCAL_HMAC_KEY_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = TestLocalHmacKeyOverride::Set(value);
    });
}

#[cfg(test)]
pub fn clear_test_local_hmac_key_override() {
    TEST_LOCAL_HMAC_KEY_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = TestLocalHmacKeyOverride::Unset;
    });
}

/// Segment ID - sequential number identifying a WAL segment file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SegmentId(u64);

impl SegmentId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the next segment ID.
    /// Uses saturating arithmetic on overflow.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Return the next segment ID, or `None` if the segment namespace is exhausted.
    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Returns the filename for this segment: `wal_000000000001.log`
    pub fn filename(self) -> String {
        format!("wal_{:012}.log", self.0)
    }

    /// Parse a segment ID from a filename. Returns `None` if the name is not
    /// a valid segment filename.
    pub fn from_filename(name: &str) -> Option<Self> {
        let name = name.strip_prefix("wal_")?.strip_suffix(".log")?;
        name.parse::<u64>().ok().map(Self)
    }
}

/// Build the full path for a segment file in the given directory.
fn segment_path(dir: &Path, id: SegmentId) -> PathBuf {
    dir.join(id.filename())
}

fn recycled_segment_path(dir: &Path, id: SegmentId) -> PathBuf {
    dir.join(format!(
        "{WAL_RECYCLED_SEGMENT_PREFIX}{:012}{WAL_RECYCLED_SEGMENT_SUFFIX}",
        id.get()
    ))
}

fn recycled_segment_id_from_filename(name: &str) -> Option<SegmentId> {
    let name = name
        .strip_prefix(WAL_RECYCLED_SEGMENT_PREFIX)?
        .strip_suffix(WAL_RECYCLED_SEGMENT_SUFFIX)?;
    name.parse::<u64>().ok().map(SegmentId::new)
}

/// Map an `io::Error` to a `DbError::internal` with a WAL-specific prefix.
fn map_io(context: &str, err: std::io::Error) -> DbError {
    DbError::internal(format!("WAL I/O error: {context}: {err}"))
}

/// Flush directory metadata so segment create/remove/rename operations are
/// durable across crashes.
pub fn sync_dir(dir: &Path) -> DbResult<()> {
    #[cfg(test)]
    {
        if should_fail_dir_sync() {
            return Err(DbError::internal(
                "WAL I/O error: syncing WAL directory: injected failure",
            ));
        }
    }

    aiondb_core::bounded_io::sync_dir(dir).map_err(|e| map_io("syncing WAL directory", e))
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    if let Some(parent) = path.parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        sync_dir(parent)?;
    }
    Ok(())
}

fn sync_dir_and_parent(dir: &Path) -> DbResult<()> {
    sync_dir(dir)?;
    sync_parent_dir(dir)
}

fn copy_file_and_fsync(source: &Path, destination: &Path, context: &str) -> DbResult<()> {
    fs::copy(source, destination).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: {context}: copying {} -> {} failed: {error}",
            source.display(),
            destination.display()
        ))
    })?;
    File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: {context}: syncing copied file {} failed: {error}",
                destination.display()
            ))
        })
}

/// Return the configured WAL archive directory, if set.
///
/// Rejects paths containing `..` components to prevent directory traversal.
#[must_use]
pub fn archive_dir_from_env() -> Option<PathBuf> {
    let Ok(raw) = std::env::var(WAL_ARCHIVE_DIR_ENV) else {
        return None;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            tracing::warn!(
                "rejecting WAL archive dir containing '..' component: {}",
                path.display()
            );
            return None;
        }
    }
    Some(path)
}

/// Return whether restoring missing WAL segments from archive is enabled.
#[must_use]
pub fn restore_from_archive_enabled() -> bool {
    std::env::var(WAL_RESTORE_FROM_ARCHIVE_ENV).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Minimum accepted length for WAL integrity HMAC keys.
///
/// HMAC-SHA256 technically accepts arbitrary-length keys (it pads or hashes
/// internally), but a key shorter than the SHA-256 output is the practical
/// security ceiling and adds no resistance against forgery. Reject anything
/// below 32 bytes so we fail loudly instead of producing a weak signature.
const MIN_HMAC_KEY_BYTES: usize = 32;

fn parse_hmac_key(raw: &str, label: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() < MIN_HMAC_KEY_BYTES {
        tracing::warn!(
            "{label} is {} bytes; the minimum accepted length is {} bytes",
            trimmed.len(),
            MIN_HMAC_KEY_BYTES
        );
        return None;
    }
    Some(trimmed.as_bytes().to_vec())
}

fn parse_archive_hmac_key(raw: &str) -> Option<Vec<u8>> {
    parse_hmac_key(raw, "WAL archive HMAC key")
}

fn archive_hmac_key_from_env() -> Option<Vec<u8>> {
    let Ok(raw) = std::env::var(WAL_ARCHIVE_HMAC_KEY_ENV) else {
        return None;
    };
    parse_archive_hmac_key(&raw)
}

fn local_hmac_key_from_env() -> Option<Vec<u8>> {
    #[cfg(test)]
    if let TestLocalHmacKeyOverride::Set(override_value) =
        TEST_LOCAL_HMAC_KEY_OVERRIDE.with(|slot| slot.borrow().clone())
    {
        return override_value;
    }
    if let Ok(raw) = std::env::var(WAL_LOCAL_HMAC_KEY_ENV) {
        return parse_hmac_key(&raw, "WAL local integrity HMAC key");
    }
    archive_hmac_key_from_env()
}

fn archive_hmac_key_required() -> DbResult<Vec<u8>> {
    archive_hmac_key_from_env().ok_or_else(|| {
        DbError::internal(format!(
            "WAL archive integrity key required: set {WAL_ARCHIVE_HMAC_KEY_ENV}"
        ))
    })
}

fn local_hmac_key_required() -> DbResult<Vec<u8>> {
    local_hmac_key_from_env().ok_or_else(|| {
        DbError::internal(format!(
            "WAL local integrity key required: set {WAL_LOCAL_HMAC_KEY_ENV} (or reuse {WAL_ARCHIVE_HMAC_KEY_ENV})"
        ))
    })
}

fn archive_hmac_path(archive_dir: &Path, id: SegmentId) -> PathBuf {
    archive_dir.join(format!("{}{}", id.filename(), WAL_ARCHIVE_HMAC_SUFFIX))
}

fn local_hmac_path(dir: &Path, id: SegmentId) -> PathBuf {
    dir.join(format!("{}{}", id.filename(), WAL_LOCAL_HMAC_SUFFIX))
}

fn compute_archive_hmac(mut reader: impl Read, key: &[u8]) -> DbResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| DbError::internal("WAL archive integrity key is invalid"))?;
    let mut buffer = vec![0u8; WAL_HMAC_STREAM_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: archiving WAL segment: streaming read failed: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        mac.update(&buffer[..read]);
    }
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_state_from_bytes(key: &[u8], data: &[u8]) -> DbResult<HmacSha256> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| DbError::internal("WAL local integrity key is invalid"))?;
    mac.update(data);
    Ok(mac)
}

fn write_local_segment_hmac(
    dir: &Path,
    id: SegmentId,
    trusted_len: u64,
    tag: &[u8],
) -> DbResult<()> {
    let sidecar_path = local_hmac_path(dir, id);
    let sidecar_existed = sidecar_path.exists();
    let mut payload = Vec::with_capacity(WAL_LOCAL_HMAC_SIZE_BYTES);
    payload.extend_from_slice(WAL_LOCAL_HMAC_MAGIC);
    payload.push(WAL_LOCAL_HMAC_VERSION);
    payload.extend_from_slice(&trusted_len.to_le_bytes());
    payload.extend_from_slice(tag);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&sidecar_path)
        .map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: writing local WAL integrity sidecar {} failed: {error}",
                sidecar_path.display()
            ))
        })?;
    file.write_all(&payload).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: writing local WAL integrity sidecar {} failed: {error}",
            sidecar_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: syncing local WAL integrity sidecar {} failed: {error}",
            sidecar_path.display()
        ))
    })?;
    if sidecar_existed {
        Ok(())
    } else {
        sync_dir_and_parent(dir)
    }
}

fn read_local_segment_hmac(dir: &Path, id: SegmentId) -> DbResult<(u64, Vec<u8>)> {
    let sidecar_path = local_hmac_path(dir, id);
    let bytes = read_exact_sized_file(
        &sidecar_path,
        WAL_LOCAL_HMAC_SIZE_BYTES,
        "local WAL integrity sidecar",
    )?;
    if &bytes[..WAL_LOCAL_HMAC_MAGIC.len()] != WAL_LOCAL_HMAC_MAGIC {
        return Err(DbError::internal(format!(
            "WAL local integrity sidecar {} has invalid magic",
            sidecar_path.display()
        )));
    }
    if bytes[WAL_LOCAL_HMAC_MAGIC.len()] != WAL_LOCAL_HMAC_VERSION {
        return Err(DbError::internal(format!(
            "WAL local integrity sidecar {} has unsupported version {}",
            sidecar_path.display(),
            bytes[WAL_LOCAL_HMAC_MAGIC.len()]
        )));
    }
    let length_offset = WAL_LOCAL_HMAC_MAGIC.len() + 1;
    let trusted_len = u64::from_le_bytes(
        bytes[length_offset..length_offset + 8]
            .try_into()
            .map_err(|_| DbError::internal("WAL local integrity sidecar length is malformed"))?,
    );
    Ok((trusted_len, bytes[length_offset + 8..].to_vec()))
}

fn write_archive_segment_hmac(archive_dir: &Path, id: SegmentId, key: &[u8]) -> DbResult<()> {
    let segment_path = segment_path(archive_dir, id);
    let segment_file = File::open(&segment_path).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: archiving WAL segment: opening archived segment {} failed: {error}",
            segment_path.display()
        ))
    })?;
    let tag = compute_archive_hmac(segment_file, key)?;
    let tag_path = archive_hmac_path(archive_dir, id);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tag_path)
        .map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: archiving WAL segment: opening HMAC sidecar {} failed: {error}",
                tag_path.display()
            ))
        })?;
    file.write_all(&tag).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: archiving WAL segment: writing HMAC sidecar {} failed: {error}",
            tag_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: archiving WAL segment: syncing HMAC sidecar {} failed: {error}",
            tag_path.display()
        ))
    })?;
    sync_dir_and_parent(archive_dir)
}

fn verify_archive_segment_hmac(archive_dir: &Path, id: SegmentId, key: &[u8]) -> DbResult<()> {
    let segment_file_path = segment_path(archive_dir, id);
    let signature_path = archive_hmac_path(archive_dir, id);
    let segment_file = File::open(&segment_file_path).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: restoring archived WAL segment: opening archived segment {} failed: {error}",
            segment_file_path.display()
        ))
    })?;
    let signature =
        read_exact_sized_file(&signature_path, WAL_HMAC_TAG_BYTES, "archive HMAC sidecar")?;
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| DbError::internal("WAL archive integrity key is invalid"))?;
    let mut buffer = vec![0u8; WAL_HMAC_STREAM_BUFFER_BYTES];
    let mut reader = segment_file;
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: restoring archived WAL segment: reading archived segment {} failed: {error}",
                segment_file_path.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        mac.update(&buffer[..read]);
    }
    mac.verify_slice(&signature).map_err(|_| {
        DbError::internal(format!(
            "WAL archive integrity verification failed for segment {}",
            id.get()
        ))
    })
}

fn read_exact_sized_file(path: &Path, expected_len: usize, context: &str) -> DbResult<Vec<u8>> {
    let file = File::open(path).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: reading {context} {} failed: {error}",
            path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "WAL I/O error: reading {context} metadata {} failed: {error}",
                path.display()
            ))
        })?
        .len();
    let expected_len_u64 = u64::try_from(expected_len).map_err(|_| {
        DbError::internal(format!(
            "WAL {context} {} expected size does not fit in u64",
            path.display()
        ))
    })?;
    if file_len != expected_len_u64 {
        return Err(DbError::internal(format!(
            "WAL {context} {} has unexpected size {file_len}, expected {expected_len}",
            path.display()
        )));
    }

    let mut bytes = Vec::with_capacity(expected_len);
    let mut limited = file.take(expected_len_u64.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: reading {context} {} failed: {error}",
            path.display()
        ))
    })?;
    if bytes.len() != expected_len {
        return Err(DbError::internal(format!(
            "WAL {context} {} changed while reading; expected {expected_len} bytes, got {}",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes)
}

impl LocalWalAuthState {
    pub(crate) fn from_existing_segment_bytes(data: &[u8]) -> DbResult<Option<Self>> {
        let Some(key) = local_hmac_key_from_env() else {
            return Ok(None);
        };
        Ok(Some(Self {
            mac: hmac_state_from_bytes(&key, data)?,
        }))
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.mac.update(bytes);
    }

    pub(crate) fn persist(&self, dir: &Path, id: SegmentId, trusted_len: u64) -> DbResult<()> {
        let tag = self.mac.clone().finalize().into_bytes().to_vec();
        write_local_segment_hmac(dir, id, trusted_len, &tag)
    }
}

pub fn verify_local_segment_integrity_if_configured(
    dir: &Path,
    id: SegmentId,
    is_last_segment: bool,
    data: &[u8],
) -> DbResult<LocalWalAuthVerification> {
    if local_hmac_key_from_env().is_none() {
        return Ok(LocalWalAuthVerification {
            trusted_len: u64::try_from(data.len()).unwrap_or(u64::MAX),
            truncated_unauthenticated_tail: false,
        });
    }

    let actual_len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let header = parse_segment_header(data)?;
    let empty_segment_len = u64::try_from(header.entry_offset).unwrap_or(u64::MAX);

    let sidecar_path = local_hmac_path(dir, id);
    if !sidecar_path.exists() {
        if is_last_segment {
            return Ok(LocalWalAuthVerification {
                trusted_len: empty_segment_len,
                truncated_unauthenticated_tail: actual_len > empty_segment_len,
            });
        }
        return Err(DbError::internal(format!(
            "WAL local integrity sidecar missing for archived segment {}",
            id.get()
        )));
    }

    let (trusted_len, expected_tag) = match read_local_segment_hmac(dir, id) {
        Ok(sidecar) => sidecar,
        Err(error) => {
            return Err(DbError::internal(format!(
                "WAL local integrity sidecar missing or unreadable for segment {}: {error}",
                id.get()
            )));
        }
    };

    if trusted_len > actual_len {
        return Err(DbError::internal(format!(
            "WAL local integrity sidecar for segment {} records {} bytes but file only has {} bytes",
            id.get(),
            trusted_len,
            actual_len
        )));
    }

    let trusted_usize = usize::try_from(trusted_len)
        .map_err(|_| DbError::internal("WAL local integrity trusted length exceeds usize"))?;
    let key = local_hmac_key_required()?;
    let computed = hmac_state_from_bytes(&key, &data[..trusted_usize])?
        .finalize()
        .into_bytes()
        .to_vec();
    if computed != expected_tag {
        return Err(DbError::internal(format!(
            "WAL local integrity verification failed for segment {}",
            id.get()
        )));
    }

    if actual_len > trusted_len {
        if !is_last_segment {
            return Err(DbError::internal(format!(
                "WAL archived segment {} contains unauthenticated trailing bytes",
                id.get()
            )));
        }
        return Ok(LocalWalAuthVerification {
            trusted_len,
            truncated_unauthenticated_tail: true,
        });
    }

    Ok(LocalWalAuthVerification {
        trusted_len,
        truncated_unauthenticated_tail: false,
    })
}

pub fn verify_local_wal_dir_if_configured(wal_dir: &Path) -> DbResult<()> {
    if local_hmac_key_from_env().is_none() || !wal_dir.exists() {
        return Ok(());
    }
    let segments = list_segments_if_exists(wal_dir)?;
    let last = segments.last().copied();
    for seg_id in segments {
        let data = read_segment_bytes_bounded(
            wal_dir,
            seg_id,
            WAL_SEGMENT_SCAN_HARD_LIMIT_BYTES,
            "doctor local WAL integrity",
        )?;
        let verification = verify_local_segment_integrity_if_configured(
            wal_dir,
            seg_id,
            Some(seg_id) == last,
            &data,
        )?;
        if verification.truncated_unauthenticated_tail {
            return Err(DbError::internal(format!(
                "WAL local integrity detected unauthenticated trailing bytes in last segment {}",
                seg_id.get()
            )));
        }
    }
    Ok(())
}

/// List WAL segments in `dir`, returning an empty list when the directory is
/// absent.
pub fn list_segments_if_exists(dir: &Path) -> DbResult<Vec<SegmentId>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut ids: Vec<SegmentId> = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name();
                    let name_str = name.to_str()?;
                    SegmentId::from_filename(name_str)
                })
                .collect();
            ids.sort();
            Ok(ids)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(map_io("listing segments", error)),
    }
}

/// Copy a WAL segment from the local WAL directory into `archive_dir`.
pub fn archive_segment_to_dir(wal_dir: &Path, archive_dir: &Path, id: SegmentId) -> DbResult<()> {
    fs::create_dir_all(archive_dir).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: creating archive directory {} failed: {error}",
            archive_dir.display()
        ))
    })?;
    sync_dir_and_parent(archive_dir)?;

    let source = segment_path(wal_dir, id);
    let destination = segment_path(archive_dir, id);

    if !source.exists() {
        return Ok(());
    }

    copy_file_and_fsync(&source, &destination, "archiving WAL segment")?;
    sync_dir(archive_dir)
}

/// Restore a missing WAL segment from `archive_dir` into `wal_dir`.
///
/// Returns `true` if the segment was restored or already present locally,
/// `false` when the archive copy is unavailable.
pub fn restore_segment_from_dir(
    wal_dir: &Path,
    archive_dir: &Path,
    id: SegmentId,
) -> DbResult<bool> {
    let destination = segment_path(wal_dir, id);
    if destination.exists() {
        return Ok(true);
    }

    let source = segment_path(archive_dir, id);
    if !source.exists() {
        return Ok(false);
    }

    ensure_wal_dir(wal_dir)?;
    copy_file_and_fsync(&source, &destination, "restoring archived WAL segment")?;
    sync_dir_and_parent(wal_dir)?;
    Ok(true)
}

/// Archive the segment if `AIONDB_WAL_ARCHIVE_DIR` is configured.
pub fn archive_segment_if_configured(wal_dir: &Path, id: SegmentId) -> DbResult<()> {
    let Some(archive_dir) = archive_dir_from_env() else {
        return Ok(());
    };
    let key = archive_hmac_key_required()?;
    archive_segment_to_dir(wal_dir, &archive_dir, id)?;
    write_archive_segment_hmac(&archive_dir, id, &key)
}

/// Attempt to restore the segment from archive when restore mode is enabled.
pub fn restore_segment_if_configured(wal_dir: &Path, id: SegmentId) -> DbResult<bool> {
    if !restore_from_archive_enabled() {
        return Ok(false);
    }
    let Some(archive_dir) = archive_dir_from_env() else {
        return Ok(false);
    };
    let destination = segment_path(wal_dir, id);
    if destination.exists() {
        return Ok(true);
    }
    let source = segment_path(&archive_dir, id);
    if !source.exists() {
        return Ok(false);
    }
    let key = archive_hmac_key_required()?;
    verify_archive_segment_hmac(&archive_dir, id, &key)?;
    restore_segment_from_dir(wal_dir, &archive_dir, id)
}

/// Return the byte offset where WAL entries start in a segment buffer.
///
/// New segments start with an explicit `magic + version` header. Legacy
/// segments have no header and start directly with encoded WAL entries.
pub(crate) fn parse_segment_header(data: &[u8]) -> DbResult<SegmentHeaderInfo> {
    if data.len() >= SEGMENT_HEADER_SIZE && has_segment_magic(data) {
        let version = data[SEGMENT_MAGIC.len()];
        match version {
            SEGMENT_FORMAT_VERSION_V1 => {
                return Ok(SegmentHeaderInfo {
                    format_version: Some(SEGMENT_FORMAT_VERSION_V1),
                    entry_offset: SEGMENT_HEADER_SIZE_V1,
                    lsn_mode: None,
                    system_identifier: None,
                    timeline_id: None,
                });
            }
            SEGMENT_FORMAT_VERSION_V2 => {
                let lsn_mode = SegmentLsnMode::from_byte(data[SEGMENT_MAGIC.len() + 1])
                    .ok_or_else(|| {
                        DbError::internal("WAL segment header contains an unknown LSN mode")
                    })?;
                return Ok(SegmentHeaderInfo {
                    format_version: Some(SEGMENT_FORMAT_VERSION_V2),
                    entry_offset: SEGMENT_HEADER_SIZE_V2,
                    lsn_mode: Some(lsn_mode),
                    system_identifier: None,
                    timeline_id: None,
                });
            }
            SEGMENT_FORMAT_VERSION => {
                let lsn_mode = SegmentLsnMode::from_byte(data[SEGMENT_MAGIC.len() + 1])
                    .ok_or_else(|| {
                        DbError::internal("WAL segment header contains an unknown LSN mode")
                    })?;
                let mut system_identifier_bytes = [0u8; 8];
                system_identifier_bytes
                    .copy_from_slice(&data[SEGMENT_MAGIC.len() + 2..SEGMENT_MAGIC.len() + 10]);
                let mut timeline_bytes = [0u8; 4];
                timeline_bytes
                    .copy_from_slice(&data[SEGMENT_MAGIC.len() + 10..SEGMENT_MAGIC.len() + 14]);
                let system_identifier = u64::from_le_bytes(system_identifier_bytes);
                let timeline_id = u32::from_le_bytes(timeline_bytes);
                return Ok(SegmentHeaderInfo {
                    format_version: Some(SEGMENT_FORMAT_VERSION),
                    entry_offset: SEGMENT_HEADER_SIZE,
                    lsn_mode: Some(lsn_mode),
                    system_identifier: (system_identifier != 0).then_some(system_identifier),
                    timeline_id: (timeline_id != 0).then_some(timeline_id),
                });
            }
            _ => {
                return Err(DbError::internal(format!(
                    "WAL segment format version {version} is not supported (expected {SEGMENT_FORMAT_VERSION_V1}, {SEGMENT_FORMAT_VERSION_V2} or {SEGMENT_FORMAT_VERSION})"
                )));
            }
        }
    }

    if data.len() >= SEGMENT_MAGIC.len()
        && data.len() < SEGMENT_HEADER_SIZE_V1
        && has_segment_magic(data)
    {
        return Err(DbError::internal("WAL segment header is truncated"));
    }

    if data.len() >= SEGMENT_HEADER_SIZE_V1
        && data.len() < SEGMENT_HEADER_SIZE
        && has_segment_magic(data)
    {
        let version = data[SEGMENT_MAGIC.len()];
        if version == SEGMENT_FORMAT_VERSION_V1 {
            return Ok(SegmentHeaderInfo {
                format_version: Some(SEGMENT_FORMAT_VERSION_V1),
                entry_offset: SEGMENT_HEADER_SIZE_V1,
                lsn_mode: None,
                system_identifier: None,
                timeline_id: None,
            });
        }
        if version == SEGMENT_FORMAT_VERSION_V2 {
            if data.len() >= SEGMENT_HEADER_SIZE_V2 {
                let lsn_mode = SegmentLsnMode::from_byte(data[SEGMENT_MAGIC.len() + 1])
                    .ok_or_else(|| {
                        DbError::internal("WAL segment header contains an unknown LSN mode")
                    })?;
                return Ok(SegmentHeaderInfo {
                    format_version: Some(SEGMENT_FORMAT_VERSION_V2),
                    entry_offset: SEGMENT_HEADER_SIZE_V2,
                    lsn_mode: Some(lsn_mode),
                    system_identifier: None,
                    timeline_id: None,
                });
            }
            return Err(DbError::internal("WAL segment header is truncated"));
        }
        if version == SEGMENT_FORMAT_VERSION {
            return Err(DbError::internal("WAL segment header is truncated"));
        }
    }

    Ok(SegmentHeaderInfo {
        format_version: None,
        entry_offset: 0,
        lsn_mode: None,
        system_identifier: None,
        timeline_id: None,
    })
}

pub fn entry_data_offset(data: &[u8]) -> DbResult<usize> {
    Ok(parse_segment_header(data)?.entry_offset)
}

pub fn inspect_segment_header(data: &[u8]) -> DbResult<SegmentHeaderSummary> {
    Ok(parse_segment_header(data)?.into())
}

#[cfg(test)]
pub(crate) fn inject_dir_sync_failure() {
    inject_dir_sync_failure_after(0);
}

#[cfg(test)]
pub(crate) fn inject_dir_sync_failure_after(successful_syncs: usize) {
    FAIL_DIR_SYNC_COUNTDOWN.with(|countdown| countdown.set(Some(successful_syncs)));
}

#[cfg(test)]
pub(crate) fn reset_dir_sync_failure_injection() {
    FAIL_DIR_SYNC_COUNTDOWN.with(|countdown| countdown.set(None));
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    reset_dir_sync_failure_injection();
    let mut dir = std::env::temp_dir();
    let unique = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.push(format!(
        "aiondb_wal_test_pid{}_tid{:?}_{unique}_{name}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// List all segment files in a directory, sorted by ID.
pub fn list_segments(dir: &Path) -> DbResult<Vec<SegmentId>> {
    let entries = fs::read_dir(dir).map_err(|e| map_io("listing segments", e))?;

    let mut ids: Vec<SegmentId> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name_str = name.to_str()?;
            SegmentId::from_filename(name_str)
        })
        .collect();

    ids.sort();
    Ok(ids)
}

fn list_recycled_segments(dir: &Path) -> DbResult<Vec<SegmentId>> {
    let mut recycled = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| map_io("listing recycled segments", e))? {
        let entry = entry.map_err(|e| map_io("reading recycled segment entry", e))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if let Some(id) = recycled_segment_id_from_filename(&file_name) {
            recycled.push(id);
        }
    }
    recycled.sort();
    Ok(recycled)
}

/// Open (or create) a segment file for appending.
pub fn open_segment_for_append(
    dir: &Path,
    id: SegmentId,
    wal_lsn_mode: WalLsnMode,
) -> DbResult<File> {
    let path = segment_path(dir, id);
    let identity = resolve_cluster_identity_from_wal_dir(dir);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| map_io("opening segment for append", e))?;

    if fs::metadata(&path)
        .map_err(|e| map_io("reading segment metadata", e))?
        .len()
        == 0
    {
        let mut file = file;
        file.write_all(&segment_header_bytes(
            SegmentLsnMode::from(wal_lsn_mode),
            identity,
        ))
        .map_err(|e| map_io("writing segment header", e))?;
        file.flush()
            .map_err(|e| map_io("flushing segment header", e))?;
        sync_dir_and_parent(dir)?;
        return Ok(file);
    }

    sync_dir_and_parent(dir)?;
    Ok(file)
}

pub fn recycle_segment(dir: &Path, id: SegmentId) -> DbResult<()> {
    let source = segment_path(dir, id);
    let target = recycled_segment_path(dir, id);
    fs::rename(&source, &target).map_err(|e| map_io("recycling segment", e))?;
    let sidecar = local_hmac_path(dir, id);
    if sidecar.exists() {
        fs::remove_file(&sidecar).map_err(|e| map_io("removing local WAL integrity sidecar", e))?;
    }
    sync_dir_and_parent(dir)
}

pub fn open_recycled_segment_for_append(
    dir: &Path,
    new_id: SegmentId,
    wal_lsn_mode: WalLsnMode,
) -> DbResult<Option<File>> {
    let Some(recycled_id) = list_recycled_segments(dir)?.into_iter().next() else {
        return Ok(None);
    };

    let recycled_path = recycled_segment_path(dir, recycled_id);
    let target_path = segment_path(dir, new_id);
    let target_sidecar_path = local_hmac_path(dir, new_id);
    let identity = resolve_cluster_identity_from_wal_dir(dir);
    let mut recycled_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&recycled_path)
        .map_err(|e| map_io("opening recycled segment", e))?;
    recycled_file
        .set_len(0)
        .map_err(|e| map_io("truncating recycled segment", e))?;
    recycled_file
        .write_all(&segment_header_bytes(
            SegmentLsnMode::from(wal_lsn_mode),
            identity,
        ))
        .map_err(|e| map_io("writing recycled segment header", e))?;
    recycled_file
        .flush()
        .map_err(|e| map_io("flushing recycled segment header", e))?;
    recycled_file
        .sync_all()
        .map_err(|e| map_io("syncing recycled segment header", e))?;
    drop(recycled_file);

    fs::rename(&recycled_path, &target_path)
        .map_err(|e| map_io("promoting recycled segment", e))?;
    if target_sidecar_path.exists() {
        fs::remove_file(&target_sidecar_path)
            .map_err(|e| map_io("removing recycled local WAL integrity sidecar", e))?;
    }
    sync_dir_and_parent(dir)?;

    OpenOptions::new()
        .append(true)
        .open(&target_path)
        .map(Some)
        .map_err(|e| map_io("opening recycled segment for append", e))
}

/// Open a segment file for reading.
pub fn open_segment_for_read(dir: &Path, id: SegmentId) -> DbResult<File> {
    File::open(segment_path(dir, id)).map_err(|e| map_io("opening segment for read", e))
}

/// Get the current size of a segment file in bytes.
pub fn segment_size(dir: &Path, id: SegmentId) -> DbResult<u64> {
    let meta =
        fs::metadata(segment_path(dir, id)).map_err(|e| map_io("reading segment size", e))?;
    Ok(meta.len())
}

/// Read an entire WAL segment into memory with an explicit size safety limit.
pub fn read_segment_bytes_bounded(
    dir: &Path,
    id: SegmentId,
    max_bytes: u64,
    context: &str,
) -> DbResult<Vec<u8>> {
    let size = segment_size(dir, id)?;
    if size > max_bytes {
        return Err(DbError::internal(format!(
            "WAL {context}: segment {} is {} bytes, exceeds safety limit of {} bytes",
            id.get(),
            size,
            max_bytes
        )));
    }

    let capacity = usize::try_from(size).map_err(|_| {
        DbError::internal(format!(
            "WAL {context}: segment {} size {} cannot be represented on this platform",
            id.get(),
            size
        ))
    })?;

    let file = open_segment_for_read(dir, id)?;
    let mut data = Vec::with_capacity(capacity);
    let mut limited_reader = file.take(max_bytes.saturating_add(1));
    limited_reader.read_to_end(&mut data).map_err(|error| {
        DbError::internal(format!(
            "WAL I/O error: {context}: reading segment failed: {error}"
        ))
    })?;
    let loaded_len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if loaded_len > max_bytes {
        return Err(DbError::internal(format!(
            "WAL {context}: segment {} grew to {} bytes during read and exceeded safety limit {} bytes",
            id.get(),
            loaded_len,
            max_bytes
        )));
    }
    Ok(data)
}

/// Remove a segment file from disk.
pub fn remove_segment(dir: &Path, id: SegmentId) -> DbResult<()> {
    fs::remove_file(segment_path(dir, id)).map_err(|e| map_io("removing segment", e))?;
    let sidecar = local_hmac_path(dir, id);
    if sidecar.exists() {
        fs::remove_file(&sidecar).map_err(|e| map_io("removing local WAL integrity sidecar", e))?;
    }
    sync_dir_and_parent(dir)
}

/// Ensure the WAL directory exists, creating it (and parents) if necessary.
pub fn ensure_wal_dir(dir: &Path) -> DbResult<()> {
    fs::create_dir_all(dir).map_err(|e| map_io("creating WAL directory", e))?;
    sync_dir_and_parent(dir)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn segment_id_filename_format() {
        assert_eq!(SegmentId::new(1).filename(), "wal_000000000001.log");
        assert_eq!(SegmentId::new(42).filename(), "wal_000000000042.log");
        assert_eq!(
            SegmentId::new(999_999_999_999).filename(),
            "wal_999999999999.log"
        );
    }

    #[test]
    fn segment_id_from_filename_valid() {
        assert_eq!(
            SegmentId::from_filename("wal_000000000001.log"),
            Some(SegmentId::new(1))
        );
        assert_eq!(
            SegmentId::from_filename("wal_000000000042.log"),
            Some(SegmentId::new(42))
        );
    }

    #[test]
    fn segment_id_from_filename_invalid() {
        assert_eq!(SegmentId::from_filename("not_a_segment.log"), None);
        assert_eq!(SegmentId::from_filename("wal_abc.log"), None);
        assert_eq!(SegmentId::from_filename("wal_000000000001.txt"), None);
        assert_eq!(SegmentId::from_filename("random_file"), None);
        assert_eq!(SegmentId::from_filename(""), None);
    }

    #[test]
    fn segment_id_ordering() {
        let a = SegmentId::new(1);
        let b = SegmentId::new(2);
        let c = SegmentId::new(3);
        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
        assert_eq!(SegmentId::new(5), SegmentId::new(5));
    }

    #[test]
    fn segment_id_checked_next_reports_overflow() {
        assert_eq!(
            SegmentId::new(u64::MAX - 1).checked_next(),
            Some(SegmentId::new(u64::MAX))
        );
        assert_eq!(SegmentId::new(u64::MAX).checked_next(), None);
    }

    #[test]
    fn list_segments_empty_dir() {
        let dir = test_dir("list_empty");
        fs::create_dir_all(&dir).unwrap();
        let segments = list_segments(&dir).unwrap();
        assert!(segments.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_segments_with_files() {
        let dir = test_dir("list_with_files");
        fs::create_dir_all(&dir).unwrap();

        // Create some segment files and a non-segment file
        File::create(dir.join("wal_000000000003.log")).unwrap();
        File::create(dir.join("wal_000000000001.log")).unwrap();
        File::create(dir.join("wal_000000000002.log")).unwrap();
        File::create(dir.join("not_a_segment.txt")).unwrap();

        let segments = list_segments(&dir).unwrap();
        assert_eq!(
            segments,
            vec![SegmentId::new(1), SegmentId::new(2), SegmentId::new(3)]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_wal_dir_creates_directory() {
        let dir = test_dir("ensure_dir");
        assert!(!dir.exists());
        ensure_wal_dir(&dir).unwrap();
        assert!(dir.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_wal_dir_requires_directory_sync() {
        let dir = test_dir("ensure_dir_sync_failure");
        inject_dir_sync_failure();
        let err = ensure_wal_dir(&dir).expect_err("directory sync failure must surface");
        assert!(err.to_string().contains("syncing WAL directory"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_segment_append_creates_file() {
        let dir = test_dir("open_append");
        fs::create_dir_all(&dir).unwrap();

        let seg_id = SegmentId::new(1);
        let _file = open_segment_for_append(&dir, seg_id, WalLsnMode::Logical).unwrap();
        assert!(dir.join(seg_id.filename()).exists());

        let bytes = fs::read(dir.join(seg_id.filename())).unwrap();
        assert_eq!(
            bytes,
            segment_header_bytes(SegmentLsnMode::Logical, SegmentClusterIdentity::default())
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_data_offset_supports_v1_and_headered_segments() {
        assert_eq!(entry_data_offset(&[]).unwrap(), 0);
        assert_eq!(entry_data_offset(&[1, 2, 3, 4]).unwrap(), 0);

        let header =
            segment_header_bytes(SegmentLsnMode::Logical, SegmentClusterIdentity::default());
        assert_eq!(entry_data_offset(&header).unwrap(), SEGMENT_HEADER_SIZE);

        let mut v1_header = [0u8; SEGMENT_HEADER_SIZE_V1];
        v1_header[..SEGMENT_MAGIC.len()].copy_from_slice(SEGMENT_MAGIC);
        v1_header[SEGMENT_MAGIC.len()] = SEGMENT_FORMAT_VERSION_V1;
        assert_eq!(
            entry_data_offset(&v1_header).unwrap(),
            SEGMENT_HEADER_SIZE_V1
        );

        let err = entry_data_offset(&header[..SEGMENT_HEADER_SIZE - 1])
            .expect_err("truncated segment header must fail");
        assert!(err.to_string().contains("header is truncated"));
    }

    #[test]
    fn open_segment_append_requires_directory_sync() {
        let dir = test_dir("open_append_sync_failure");
        fs::create_dir_all(&dir).unwrap();
        inject_dir_sync_failure();
        let err = open_segment_for_append(&dir, SegmentId::new(1), WalLsnMode::Logical)
            .expect_err("segment creation must fail if directory sync fails");
        assert!(err.to_string().contains("syncing WAL directory"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_segment_append_requires_parent_directory_sync() {
        let dir = test_dir("open_append_parent_sync_failure");
        fs::create_dir_all(&dir).unwrap();

        // First sync covers the WAL directory itself; the second sync must
        // flush the parent directory metadata as well.
        inject_dir_sync_failure_after(1);
        let err = open_segment_for_append(&dir, SegmentId::new(1), WalLsnMode::Logical)
            .expect_err("segment creation must fail if parent directory sync fails");
        assert!(err.to_string().contains("syncing WAL directory"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn segment_size_after_write() {
        let dir = test_dir("seg_size");
        fs::create_dir_all(&dir).unwrap();

        let seg_id = SegmentId::new(1);
        let mut file = open_segment_for_append(&dir, seg_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"hello world").unwrap();
        file.flush().unwrap();
        drop(file);

        let size = segment_size(&dir, seg_id).unwrap();
        assert_eq!(
            size,
            aiondb_core::convert::usize_to_u64_saturating(SEGMENT_HEADER_SIZE) + 11
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_segment_deletes_file() {
        let dir = test_dir("remove_seg");
        fs::create_dir_all(&dir).unwrap();

        let seg_id = SegmentId::new(1);
        File::create(dir.join(seg_id.filename())).unwrap();
        assert!(dir.join(seg_id.filename()).exists());

        remove_segment(&dir, seg_id).unwrap();
        assert!(!dir.join(seg_id.filename()).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_segment_requires_directory_sync() {
        let dir = test_dir("remove_seg_sync_failure");
        fs::create_dir_all(&dir).unwrap();

        let seg_id = SegmentId::new(1);
        File::create(dir.join(seg_id.filename())).unwrap();

        inject_dir_sync_failure();
        let err =
            remove_segment(&dir, seg_id).expect_err("segment removal must fail if dir sync fails");
        assert!(err.to_string().contains("syncing WAL directory"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_and_restore_segment_round_trip() {
        let wal_dir = test_dir("archive_restore_wal");
        let archive_dir = test_dir("archive_restore_archive");
        ensure_wal_dir(&wal_dir).unwrap();

        let seg_id = SegmentId::new(1);
        let mut file = open_segment_for_append(&wal_dir, seg_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"hello").unwrap();
        file.flush().unwrap();
        drop(file);

        archive_segment_to_dir(&wal_dir, &archive_dir, seg_id).unwrap();
        remove_segment(&wal_dir, seg_id).unwrap();
        assert!(!wal_dir.join(seg_id.filename()).exists());

        let restored = restore_segment_from_dir(&wal_dir, &archive_dir, seg_id).unwrap();
        assert!(restored);
        assert!(wal_dir.join(seg_id.filename()).exists());

        let _ = fs::remove_dir_all(&wal_dir);
        let _ = fs::remove_dir_all(&archive_dir);
    }

    #[test]
    fn archive_hmac_verification_accepts_valid_signature() {
        let wal_dir = test_dir("archive_hmac_valid_wal");
        let archive_dir = test_dir("archive_hmac_valid_archive");
        ensure_wal_dir(&wal_dir).unwrap();

        let seg_id = SegmentId::new(1);
        let mut file = open_segment_for_append(&wal_dir, seg_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"hello").unwrap();
        file.flush().unwrap();
        drop(file);

        archive_segment_to_dir(&wal_dir, &archive_dir, seg_id).unwrap();
        write_archive_segment_hmac(&archive_dir, seg_id, b"test-integrity-key").unwrap();
        verify_archive_segment_hmac(&archive_dir, seg_id, b"test-integrity-key").unwrap();

        let _ = fs::remove_dir_all(&wal_dir);
        let _ = fs::remove_dir_all(&archive_dir);
    }

    #[test]
    fn archive_hmac_verification_rejects_tampered_segment() {
        let wal_dir = test_dir("archive_hmac_tamper_wal");
        let archive_dir = test_dir("archive_hmac_tamper_archive");
        ensure_wal_dir(&wal_dir).unwrap();

        let seg_id = SegmentId::new(1);
        let mut file = open_segment_for_append(&wal_dir, seg_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"hello").unwrap();
        file.flush().unwrap();
        drop(file);

        archive_segment_to_dir(&wal_dir, &archive_dir, seg_id).unwrap();
        write_archive_segment_hmac(&archive_dir, seg_id, b"test-integrity-key").unwrap();

        let archived_path = segment_path(&archive_dir, seg_id);
        let mut tampered = fs::read(&archived_path).unwrap();
        if let Some(last) = tampered.last_mut() {
            *last ^= 0x01;
        }
        fs::write(&archived_path, tampered).unwrap();

        let err = verify_archive_segment_hmac(&archive_dir, seg_id, b"test-integrity-key")
            .expect_err("tampered archive segment must fail HMAC verification");
        assert!(err.to_string().contains("integrity verification failed"));

        let _ = fs::remove_dir_all(&wal_dir);
        let _ = fs::remove_dir_all(&archive_dir);
    }

    #[test]
    fn recycle_segment_renames_file_out_of_active_namespace() {
        let dir = test_dir("recycle_segment");
        ensure_wal_dir(&dir).unwrap();
        let seg_id = SegmentId::new(7);
        let mut file = open_segment_for_append(&dir, seg_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"abc").unwrap();
        file.flush().unwrap();

        recycle_segment(&dir, seg_id).unwrap();

        assert!(!segment_path(&dir, seg_id).exists());
        assert!(recycled_segment_path(&dir, seg_id).exists());
    }

    #[test]
    fn open_recycled_segment_for_append_reuses_old_file_with_new_id() {
        let dir = test_dir("reuse_recycled_segment");
        ensure_wal_dir(&dir).unwrap();
        let old_id = SegmentId::new(3);
        let new_id = SegmentId::new(9);
        let mut file = open_segment_for_append(&dir, old_id, WalLsnMode::Logical).unwrap();
        file.write_all(b"payload").unwrap();
        file.flush().unwrap();
        recycle_segment(&dir, old_id).unwrap();

        let reused = open_recycled_segment_for_append(&dir, new_id, WalLsnMode::Logical)
            .unwrap()
            .expect("recycled segment should be reused");
        drop(reused);

        assert!(!recycled_segment_path(&dir, old_id).exists());
        let bytes = std::fs::read(segment_path(&dir, new_id)).unwrap();
        assert_eq!(
            bytes,
            segment_header_bytes(SegmentLsnMode::Logical, SegmentClusterIdentity::default())
        );
    }

    #[test]
    fn open_segment_header_embeds_cluster_identity_from_ancestor_replication_dir() {
        let root = test_dir("segment_header_identity");
        let wal_dir = root.join("disk").join("wal");
        std::fs::create_dir_all(root.join("replication")).unwrap();
        std::fs::write(root.join("replication").join("system_id"), b"42").unwrap();
        std::fs::write(root.join("replication").join("timeline"), b"7").unwrap();
        ensure_wal_dir(&wal_dir).unwrap();

        let seg_id = SegmentId::new(1);
        let file = open_segment_for_append(&wal_dir, seg_id, WalLsnMode::Logical).unwrap();
        drop(file);

        let bytes = std::fs::read(segment_path(&wal_dir, seg_id)).unwrap();
        let header = parse_segment_header(&bytes).unwrap();
        assert_eq!(header.lsn_mode, Some(SegmentLsnMode::Logical));
        assert_eq!(header.system_identifier, Some(42));
        assert_eq!(header.timeline_id, Some(7));
    }

    #[test]
    fn inspect_segment_header_returns_public_summary_for_v3_headers() {
        let header = segment_header_bytes(
            SegmentLsnMode::ByteOffset,
            SegmentClusterIdentity {
                system_identifier: Some(99),
                timeline_id: Some(3),
            },
        );

        let summary = inspect_segment_header(&header).unwrap();

        assert_eq!(summary.format_version, Some(SEGMENT_FORMAT_VERSION));
        assert_eq!(summary.entry_offset, SEGMENT_HEADER_SIZE);
        assert_eq!(summary.lsn_mode, Some(WalLsnMode::ByteOffset));
        assert_eq!(summary.system_identifier, Some(99));
        assert_eq!(summary.timeline_id, Some(3));
    }

    #[test]
    fn inspect_segment_header_returns_public_summary_for_v1_segments() {
        let summary = inspect_segment_header(b"v1-data").unwrap();

        assert_eq!(summary.format_version, None);
        assert_eq!(summary.entry_offset, 0);
        assert_eq!(summary.lsn_mode, None);
        assert_eq!(summary.system_identifier, None);
        assert_eq!(summary.timeline_id, None);
    }

    #[test]
    fn parse_archive_hmac_key_rejects_short_keys() {
        // A 1-byte key is technically accepted by `HmacSha256::new_from_slice`
        // but provides essentially no security against forgery. The shared
        // parser must reject keys shorter than the minimum so the env-var
        // reader fails safely instead of producing a weak signature.
        assert!(parse_archive_hmac_key("x").is_none());
        assert!(parse_archive_hmac_key("short").is_none());
        let exactly_min: String = "k".repeat(MIN_HMAC_KEY_BYTES);
        assert!(parse_archive_hmac_key(&exactly_min).is_some());
        let above_min: String = "k".repeat(MIN_HMAC_KEY_BYTES + 1);
        assert!(parse_archive_hmac_key(&above_min).is_some());
    }
}
