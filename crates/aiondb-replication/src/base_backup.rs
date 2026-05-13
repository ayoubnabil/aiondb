//! `BASE_BACKUP` bootstrap helper.
//!
//! PostgreSQL's `BASE_BACKUP` replication command streams the entire data
//! directory (or a configurable subset) over the wire so a fresh replica
//! can come up without manual file copying. AionDB ships a simpler custom
//! protocol on top of `CopyOutResponse` because we don't need PG's tar
//! format compatibility:
//!
//! ```text
//!   ┌──── BackupFrame ────────────────────────────────────────────┐
//!   │ tag(u8) │ payload_len(u32 LE)            │ payload(payload_len) │
//!   └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! Tags:
//! - `0x01 Header`     -- once at the start (`wal_start_lsn`, `timeline`).
//! - `0x02 FileStart`  -- begins a single file (`path`, `size`).
//! - `0x03 FileData`   -- a chunk of bytes for the most recently opened file.
//! - `0x04 FileEnd`    -- closes the current file (sanity-check boundary).
//! - `0x05 BackupEnd`  -- ends the backup; client should now run
//!   `START_REPLICATION` from `wal_start_lsn`.
//!
//! The frames are decoded by [`BaseBackupReader`] (consumed in order) and
//! produced by [`encode_*`] helpers (used by the primary). The replica
//! driver writes each file under the target directory mirroring the path
//! reported by the primary.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};
use aiondb_wal::Lsn;

/// First frame in the backup stream.
pub const TAG_HEADER: u8 = 0x01;
/// New-file boundary.
pub const TAG_FILE_START: u8 = 0x02;
/// File payload chunk (one or more per file).
pub const TAG_FILE_DATA: u8 = 0x03;
/// Closes the current file.
pub const TAG_FILE_END: u8 = 0x04;
/// Terminal frame.
pub const TAG_BACKUP_END: u8 = 0x05;

/// Hard cap on per-frame payload size. Matches the replication frame cap so
/// a single oversized BASE_BACKUP frame can never exhaust replica memory.
pub const MAX_BACKUP_FRAME_PAYLOAD: usize = 8 * 1024 * 1024;

/// Once-only frame describing the consistent point of the snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseBackupHeader {
    pub wal_start_lsn: Lsn,
    pub timeline: u32,
    pub system_identifier: String,
}

/// Decoded frame returned by [`BaseBackupReader::next`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackupFrame {
    Header(BaseBackupHeader),
    FileStart { path: String, size: u64 },
    FileData(Vec<u8>),
    FileEnd,
    BackupEnd,
}

// ---------------------------------------------------------------------------
// Encoders -- used by the primary
// ---------------------------------------------------------------------------

/// Encode a [`BaseBackupHeader`] frame.
pub fn encode_header(header: &BaseBackupHeader) -> DbResult<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&header.wal_start_lsn.get().to_le_bytes());
    payload.extend_from_slice(&header.timeline.to_le_bytes());
    let sysid = header.system_identifier.as_bytes();
    let sysid_len = u32::try_from(sysid.len())
        .map_err(|_| DbError::internal("BASE_BACKUP system identifier too long"))?;
    payload.extend_from_slice(&sysid_len.to_le_bytes());
    payload.extend_from_slice(sysid);
    frame(TAG_HEADER, &payload)
}

/// Encode a `FileStart` frame announcing the next file's path + size.
pub fn encode_file_start(path: &str, size: u64) -> DbResult<Vec<u8>> {
    let path_bytes = path.as_bytes();
    let path_len = u32::try_from(path_bytes.len())
        .map_err(|_| DbError::internal("BASE_BACKUP file path too long"))?;
    let mut payload = Vec::with_capacity(4 + path_bytes.len() + 8);
    payload.extend_from_slice(&path_len.to_le_bytes());
    payload.extend_from_slice(path_bytes);
    payload.extend_from_slice(&size.to_le_bytes());
    frame(TAG_FILE_START, &payload)
}

/// Encode one or more `FileData` chunks for the currently-open file.
pub fn encode_file_data(chunk: &[u8]) -> DbResult<Vec<u8>> {
    frame(TAG_FILE_DATA, chunk)
}

/// Encode a `FileEnd` frame.
pub fn encode_file_end() -> DbResult<Vec<u8>> {
    frame(TAG_FILE_END, &[])
}

/// Encode the terminal `BackupEnd` frame.
pub fn encode_backup_end() -> DbResult<Vec<u8>> {
    frame(TAG_BACKUP_END, &[])
}

fn frame(tag: u8, payload: &[u8]) -> DbResult<Vec<u8>> {
    if payload.len() > MAX_BACKUP_FRAME_PAYLOAD {
        return Err(DbError::internal(format!(
            "BASE_BACKUP frame payload {} exceeds {} bytes",
            payload.len(),
            MAX_BACKUP_FRAME_PAYLOAD
        )));
    }
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(tag);
    let len = u32::try_from(payload.len())
        .map_err(|_| DbError::internal("BASE_BACKUP frame too large"))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoder -- used by the replica
// ---------------------------------------------------------------------------

/// Decode a single `BackupFrame` from `bytes`. Returns `(frame, consumed)`
/// on success; the caller must advance its cursor by `consumed`.
pub fn decode_frame(bytes: &[u8]) -> DbResult<(BackupFrame, usize)> {
    if bytes.len() < 5 {
        return Err(DbError::protocol(
            "BASE_BACKUP frame header truncated (need ≥ 5 bytes)",
        ));
    }
    let tag = bytes[0];
    let payload_len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    if payload_len > MAX_BACKUP_FRAME_PAYLOAD {
        return Err(DbError::protocol(format!(
            "BASE_BACKUP frame payload {payload_len} exceeds {MAX_BACKUP_FRAME_PAYLOAD} bytes"
        )));
    }
    let total = 5usize
        .checked_add(payload_len)
        .ok_or_else(|| DbError::protocol("BASE_BACKUP frame length overflow"))?;
    if bytes.len() < total {
        return Err(DbError::protocol("BASE_BACKUP frame payload truncated"));
    }
    let payload = &bytes[5..total];
    let parsed = match tag {
        TAG_HEADER => {
            if payload.len() < 16 {
                return Err(DbError::protocol("BASE_BACKUP header payload too short"));
            }
            let wal_start_lsn = Lsn::new(u64::from_le_bytes(payload[0..8].try_into().unwrap()));
            let timeline = u32::from_le_bytes(payload[8..12].try_into().unwrap());
            let sysid_len = u32::from_le_bytes(payload[12..16].try_into().unwrap()) as usize;
            if payload.len() < 16 + sysid_len {
                return Err(DbError::protocol(
                    "BASE_BACKUP header system identifier truncated",
                ));
            }
            let sysid = std::str::from_utf8(&payload[16..16 + sysid_len])
                .map_err(|err| {
                    DbError::protocol(format!("BASE_BACKUP system identifier not UTF-8: {err}"))
                })?
                .to_owned();
            BackupFrame::Header(BaseBackupHeader {
                wal_start_lsn,
                timeline,
                system_identifier: sysid,
            })
        }
        TAG_FILE_START => {
            if payload.len() < 4 {
                return Err(DbError::protocol("BASE_BACKUP FileStart truncated"));
            }
            let path_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
            if payload.len() < 4 + path_len + 8 {
                return Err(DbError::protocol("BASE_BACKUP FileStart payload truncated"));
            }
            let path = std::str::from_utf8(&payload[4..4 + path_len])
                .map_err(|err| {
                    DbError::protocol(format!("BASE_BACKUP file path not UTF-8: {err}"))
                })?
                .to_owned();
            let size =
                u64::from_le_bytes(payload[4 + path_len..4 + path_len + 8].try_into().unwrap());
            BackupFrame::FileStart { path, size }
        }
        TAG_FILE_DATA => BackupFrame::FileData(payload.to_vec()),
        TAG_FILE_END => BackupFrame::FileEnd,
        TAG_BACKUP_END => BackupFrame::BackupEnd,
        other => {
            return Err(DbError::protocol(format!(
                "BASE_BACKUP unknown frame tag {other:#04x}"
            )));
        }
    };
    Ok((parsed, total))
}

// ---------------------------------------------------------------------------
// Replica-side writer
// ---------------------------------------------------------------------------

/// State machine consuming `BackupFrame`s and materialising them under
/// `target_dir`. The `Header` frame must arrive first; `BackupEnd` must
/// arrive last. Any other ordering is rejected.
pub struct BaseBackupWriter {
    target_dir: PathBuf,
    header: Option<BaseBackupHeader>,
    current_file: Option<CurrentFile>,
    completed: bool,
}

struct CurrentFile {
    file: File,
    written: u64,
    expected: u64,
    path: String,
}

impl BaseBackupWriter {
    /// Create a new writer rooted at `target_dir`. The directory is
    /// created on demand; pre-existing files under it are not removed.
    pub fn new(target_dir: PathBuf) -> DbResult<Self> {
        fs::create_dir_all(&target_dir).map_err(|err| {
            DbError::internal(format!(
                "failed to create BASE_BACKUP target dir {}: {err}",
                target_dir.display()
            ))
        })?;
        Ok(Self {
            target_dir,
            header: None,
            current_file: None,
            completed: false,
        })
    }

    /// Apply a decoded frame. Returns `true` when [`BackupFrame::BackupEnd`]
    /// has been observed (the caller should stop reading further frames).
    pub fn apply(&mut self, frame: BackupFrame) -> DbResult<bool> {
        if self.completed {
            return Err(DbError::protocol(
                "BASE_BACKUP frame received after BackupEnd",
            ));
        }
        match frame {
            BackupFrame::Header(header) => {
                if self.header.is_some() {
                    return Err(DbError::protocol("BASE_BACKUP Header sent twice"));
                }
                self.header = Some(header);
            }
            BackupFrame::FileStart { path, size } => {
                if self.header.is_none() {
                    return Err(DbError::protocol("BASE_BACKUP FileStart before Header"));
                }
                if self.current_file.is_some() {
                    return Err(DbError::protocol(
                        "BASE_BACKUP FileStart before previous FileEnd",
                    ));
                }
                let safe_path = sanitize_relative_path(&path)?;
                let full = self.target_dir.join(&safe_path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).map_err(|err| {
                        DbError::internal(format!(
                            "failed to create directory {}: {err}",
                            parent.display()
                        ))
                    })?;
                }
                let file = File::create(&full).map_err(|err| {
                    DbError::internal(format!(
                        "failed to open BASE_BACKUP destination {}: {err}",
                        full.display()
                    ))
                })?;
                self.current_file = Some(CurrentFile {
                    file,
                    written: 0,
                    expected: size,
                    path,
                });
            }
            BackupFrame::FileData(bytes) => {
                let current = self
                    .current_file
                    .as_mut()
                    .ok_or_else(|| DbError::protocol("BASE_BACKUP FileData without FileStart"))?;
                let bytes_len = bytes.len() as u64;
                if current.written + bytes_len > current.expected {
                    return Err(DbError::protocol(format!(
                        "BASE_BACKUP file \"{}\" received {} bytes but FileStart declared {}",
                        current.path,
                        current.written + bytes_len,
                        current.expected
                    )));
                }
                current.file.write_all(&bytes).map_err(|err| {
                    DbError::internal(format!(
                        "failed to write BASE_BACKUP chunk for \"{}\": {err}",
                        current.path
                    ))
                })?;
                current.written += bytes_len;
            }
            BackupFrame::FileEnd => {
                let Some(current) = self.current_file.take() else {
                    return Err(DbError::protocol(
                        "BASE_BACKUP FileEnd without matching FileStart",
                    ));
                };
                if current.written != current.expected {
                    return Err(DbError::protocol(format!(
                        "BASE_BACKUP file \"{}\" closed at {} of {} bytes",
                        current.path, current.written, current.expected
                    )));
                }
                current.file.sync_all().map_err(|err| {
                    DbError::internal(format!(
                        "failed to fsync BASE_BACKUP file \"{}\": {err}",
                        current.path
                    ))
                })?;
            }
            BackupFrame::BackupEnd => {
                if self.current_file.is_some() {
                    return Err(DbError::protocol(
                        "BASE_BACKUP BackupEnd received with an open file",
                    ));
                }
                self.completed = true;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Return the header announced by the primary at the start of the
    /// backup. Available only after the first frame has been applied.
    pub fn header(&self) -> Option<&BaseBackupHeader> {
        self.header.as_ref()
    }

    /// `true` once a `BackupEnd` frame has been applied.
    pub fn completed(&self) -> bool {
        self.completed
    }
}

fn sanitize_relative_path(path: &str) -> DbResult<PathBuf> {
    if path.is_empty() {
        return Err(DbError::protocol("BASE_BACKUP file path is empty"));
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(DbError::protocol(format!(
            "BASE_BACKUP file path \"{path}\" must be relative"
        )));
    }
    let mut safe = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => safe.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(DbError::protocol(format!(
                    "BASE_BACKUP file path \"{path}\" contains forbidden component"
                )));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(DbError::protocol(format!(
            "BASE_BACKUP file path \"{path}\" resolved to empty"
        )));
    }
    Ok(safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("aiondb-base-backup-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn frame_encode_decode_roundtrip_header() {
        let header = BaseBackupHeader {
            wal_start_lsn: Lsn::new(0x1234),
            timeline: 5,
            system_identifier: "42".to_owned(),
        };
        let encoded = encode_header(&header).expect("encode");
        let (decoded, consumed) = decode_frame(&encoded).expect("decode");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, BackupFrame::Header(header));
    }

    #[test]
    fn frame_encode_decode_roundtrip_file() {
        let start = encode_file_start("wal/0000.bin", 4).expect("file start");
        let data = encode_file_data(b"DATA").expect("file data");
        let end = encode_file_end().expect("file end");

        let (frame, _) = decode_frame(&start).expect("decode start");
        assert_eq!(
            frame,
            BackupFrame::FileStart {
                path: "wal/0000.bin".to_owned(),
                size: 4
            }
        );
        let (frame, _) = decode_frame(&data).expect("decode data");
        assert_eq!(frame, BackupFrame::FileData(b"DATA".to_vec()));
        let (frame, _) = decode_frame(&end).expect("decode end");
        assert_eq!(frame, BackupFrame::FileEnd);
    }

    #[test]
    fn writer_materialises_file_on_disk_with_correct_contents() {
        let target = temp_dir("writer_writes_file");
        let mut writer = BaseBackupWriter::new(target.clone()).expect("writer");

        let header = BaseBackupHeader {
            wal_start_lsn: Lsn::new(1),
            timeline: 1,
            system_identifier: "42".to_owned(),
        };
        writer.apply(BackupFrame::Header(header.clone())).unwrap();
        writer
            .apply(BackupFrame::FileStart {
                path: "wal/000000010000000000000001".to_owned(),
                size: 11,
            })
            .unwrap();
        writer
            .apply(BackupFrame::FileData(b"hello ".to_vec()))
            .unwrap();
        writer
            .apply(BackupFrame::FileData(b"world".to_vec()))
            .unwrap();
        writer.apply(BackupFrame::FileEnd).unwrap();
        let done = writer.apply(BackupFrame::BackupEnd).unwrap();
        assert!(done);
        assert_eq!(writer.header(), Some(&header));

        let path = target.join("wal").join("000000010000000000000001");
        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, "hello world");

        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn writer_rejects_size_mismatch() {
        let target = temp_dir("writer_size_mismatch");
        let mut writer = BaseBackupWriter::new(target.clone()).expect("writer");
        writer
            .apply(BackupFrame::Header(BaseBackupHeader {
                wal_start_lsn: Lsn::ZERO,
                timeline: 1,
                system_identifier: "42".to_owned(),
            }))
            .unwrap();
        writer
            .apply(BackupFrame::FileStart {
                path: "wal/seg".to_owned(),
                size: 4,
            })
            .unwrap();
        writer.apply(BackupFrame::FileData(b"AB".to_vec())).unwrap();
        let err = writer
            .apply(BackupFrame::FileEnd)
            .expect_err("must reject size mismatch");
        assert!(err.to_string().contains("closed at"));
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn writer_rejects_absolute_paths() {
        let target = temp_dir("writer_absolute");
        let mut writer = BaseBackupWriter::new(target.clone()).expect("writer");
        writer
            .apply(BackupFrame::Header(BaseBackupHeader {
                wal_start_lsn: Lsn::ZERO,
                timeline: 1,
                system_identifier: "42".to_owned(),
            }))
            .unwrap();
        let err = writer
            .apply(BackupFrame::FileStart {
                path: "/etc/passwd".to_owned(),
                size: 0,
            })
            .expect_err("absolute path must be rejected");
        assert!(err.to_string().contains("must be relative"));
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn writer_rejects_parent_directory_escape() {
        let target = temp_dir("writer_escape");
        let mut writer = BaseBackupWriter::new(target.clone()).expect("writer");
        writer
            .apply(BackupFrame::Header(BaseBackupHeader {
                wal_start_lsn: Lsn::ZERO,
                timeline: 1,
                system_identifier: "42".to_owned(),
            }))
            .unwrap();
        let err = writer
            .apply(BackupFrame::FileStart {
                path: "../../etc/passwd".to_owned(),
                size: 0,
            })
            .expect_err("parent escape must be rejected");
        assert!(err.to_string().contains("forbidden component"));
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn decode_frame_rejects_oversized_payload() {
        let mut buf = Vec::new();
        buf.push(TAG_HEADER);
        let huge = (MAX_BACKUP_FRAME_PAYLOAD as u32 + 1).to_le_bytes();
        buf.extend_from_slice(&huge);
        let err = decode_frame(&buf).expect_err("oversized payload must be rejected");
        assert!(err.to_string().contains("exceeds"), "{err}");
    }
}
