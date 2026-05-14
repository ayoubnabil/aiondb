#![allow(
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::return_self_not_must_use,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::uninlined_format_args
)]

pub mod codec;
pub mod lsn;
pub mod reader;
pub mod record;
pub mod replication;
pub mod segment;
pub mod writer;

use std::path::PathBuf;

pub use aiondb_tx::IsolationLevel;
pub use lsn::Lsn;
pub use reader::WalReader;
pub use record::WalRecord;
pub use writer::WalWriter;

/// Default maximum size of a single WAL segment file (16 MiB).
const DEFAULT_SEGMENT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Default group-commit batching delay (1 ms expressed in microseconds).
const DEFAULT_GROUP_COMMIT_DELAY_MICROS: u64 = 1000;

/// WAL entry payload compression mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WalCompression {
    /// Do not compress WAL entry payloads.
    #[default]
    None,
    /// Compress WAL entry payloads with LZ4.
    Lz4,
    /// Compress WAL entry payloads with Zstandard.
    Zstd,
}

impl WalCompression {
    /// Parse a compression mode from a string value (case-insensitive).
    pub fn from_str_value(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" | "off" | "false" | "0" => Some(Self::None),
            "lz4" => Some(Self::Lz4),
            "zstd" | "zst" => Some(Self::Zstd),
            _ => None,
        }
    }

    /// Return the lowercase textual name of this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lz4 => "lz4",
            Self::Zstd => "zstd",
        }
    }
}

/// WAL LSN progression mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WalLsnMode {
    /// Legacy sequential counter (`+1` per record).
    Logical,
    /// Byte-offset progression (`+encoded_entry_bytes` per record).
    #[default]
    ByteOffset,
}

impl WalLsnMode {
    /// Parse an LSN progression mode from a string value (case-insensitive).
    pub fn from_str_value(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "logical" | "counter" | "seq" | "sequential" => Some(Self::Logical),
            "byte" | "byte_offset" | "offset" | "bytes" => Some(Self::ByteOffset),
            _ => None,
        }
    }

    /// Return the lowercase textual name of this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Logical => "logical",
            Self::ByteOffset => "byte_offset",
        }
    }
}

/// Configuration for the WAL subsystem.
#[derive(Clone, Debug)]
pub struct WalConfig {
    /// Directory where WAL segment files are stored.
    pub dir: PathBuf,
    /// Maximum size of a single WAL segment file in bytes (default: 16 MiB).
    pub segment_max_bytes: u64,
    /// Whether to fsync after every flush (default: true).
    pub sync_on_flush: bool,
    /// Maximum delay in microseconds to batch concurrent commits into a single
    /// fsync (group commit). When set to 0 the group commit path is disabled
    /// and every commit flushes independently.
    /// Default: 1000 (1 ms).
    pub group_commit_delay_micros: u64,
    /// Compression mode for WAL entry payloads.
    ///
    /// `none` keeps the uncompressed entry encoding. `lz4` and `zstd`
    /// compress the record payload when it is beneficial.
    pub wal_compression: WalCompression,
    /// LSN progression mode used when assigning new WAL positions.
    pub wal_lsn_mode: WalLsnMode,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("./wal"),
            segment_max_bytes: DEFAULT_SEGMENT_MAX_BYTES,
            sync_on_flush: true,
            group_commit_delay_micros: DEFAULT_GROUP_COMMIT_DELAY_MICROS,
            wal_compression: WalCompression::None,
            wal_lsn_mode: WalLsnMode::ByteOffset,
        }
    }
}
