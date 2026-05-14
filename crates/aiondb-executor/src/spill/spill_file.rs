//! Temporary file for spilling rows to disk.
//!
//! Rows are serialised with `bincode` into a buffered file.  The writer
//! records a row count header so the reader knows when to stop.  The
//! file is deleted automatically when the last handle is dropped.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use aiondb_core::Row;
use aiondb_core::{DbError, DbResult};
use serde::{de::DeserializeOwned, Serialize};

/// Monotonic counter for unique temp file names within a process.
static SPILL_SEQ: AtomicU64 = AtomicU64::new(0);

/// Resolve the temp directory for spill files.
///
/// Uses `server_data_dir/tmp` when available, otherwise falls back to
/// the OS temp directory. Creates the directory if it does not exist.
pub(crate) fn resolve_spill_dir(server_data_dir: Option<&Path>) -> DbResult<PathBuf> {
    let dir = match server_data_dir {
        Some(base) => base.join("tmp"),
        None => std::env::temp_dir().join("aiondb_spill"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        DbError::internal(format!(
            "failed to create spill directory {}: {e}",
            dir.display()
        ))
    })?;
    Ok(dir)
}

fn make_spill_path(dir: &Path) -> PathBuf {
    let seq = SPILL_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    dir.join(format!("spill_{pid}_{seq}.tmp"))
}

fn create_spill_file(spill_dir: &Path) -> DbResult<(PathBuf, File)> {
    const MAX_CREATE_ATTEMPTS: u32 = 1024;
    for _ in 0..MAX_CREATE_ATTEMPTS {
        let path = make_spill_path(spill_dir);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to create spill file {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Err(DbError::internal(format!(
        "failed to allocate unique spill file in {} after {MAX_CREATE_ATTEMPTS} attempts",
        spill_dir.display()
    )))
}

// -----------------------------------------------------------------------
// Writer
// -----------------------------------------------------------------------

pub(crate) struct SpillWriter<T> {
    writer: BufWriter<File>,
    /// `None` after `into_reader()` has transferred ownership to the reader.
    path: Option<PathBuf>,
    row_count: u64,
    byte_count: u64,
    marker: PhantomData<T>,
}

impl<T> SpillWriter<T>
where
    T: Serialize,
{
    /// Create a new spill file in the given directory.
    pub(crate) fn new(spill_dir: &Path) -> DbResult<Self> {
        let (path, file) = create_spill_file(spill_dir)?;
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        // Reserve space for the row count header (written on finish).
        writer.write_all(&0u64.to_le_bytes()).map_err(spill_io)?;
        Ok(Self {
            writer,
            path: Some(path),
            row_count: 0,
            byte_count: 8,
            marker: PhantomData,
        })
    }

    /// Serialise and append one item.
    pub(crate) fn write_item(&mut self, item: &T) -> DbResult<()> {
        let encoded = bincode::serialize(item)
            .map_err(|e| DbError::internal(format!("spill encode: {e}")))?;
        let len = u32::try_from(encoded.len())
            .map_err(|_| DbError::internal("spill item exceeds u32 length prefix"))?;
        self.writer
            .write_all(&len.to_le_bytes())
            .map_err(spill_io)?;
        self.writer.write_all(&encoded).map_err(spill_io)?;
        self.row_count += 1;
        self.byte_count += 4 + encoded.len() as u64;
        Ok(())
    }

    /// Approximate bytes written to disk.
    pub(crate) fn byte_count(&self) -> u64 {
        self.byte_count
    }

    /// Finish writing and produce a reader positioned at the first row.
    ///
    /// The temp file ownership transfers to the reader; the writer's
    /// drop will no longer try to delete it.
    pub(crate) fn into_reader(mut self) -> DbResult<SpillReader<T>> {
        let path = self
            .path
            .take()
            .ok_or_else(|| DbError::internal("spill writer already consumed"))?;

        // Flush and reopen for reading.
        self.writer.flush().map_err(spill_io)?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(spill_io)?;
        // Write the final row count at offset 0.
        file.seek(SeekFrom::Start(0)).map_err(spill_io)?;
        file.write_all(&self.row_count.to_le_bytes())
            .map_err(spill_io)?;
        // Position at the first row (after the 8-byte header).
        file.seek(SeekFrom::Start(8)).map_err(spill_io)?;

        let reader = BufReader::with_capacity(64 * 1024, file);
        Ok(SpillReader {
            reader,
            remaining: self.row_count,
            path,
            marker: PhantomData,
        })
    }
}

impl<T> Drop for SpillWriter<T> {
    fn drop(&mut self) {
        if let Some(ref path) = self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

// -----------------------------------------------------------------------
// Reader
// -----------------------------------------------------------------------

pub(crate) struct SpillReader<T> {
    reader: BufReader<File>,
    remaining: u64,
    path: PathBuf,
    marker: PhantomData<T>,
}

impl<T> SpillReader<T>
where
    T: DeserializeOwned,
{
    /// Read the next item, or `None` when all items have been consumed.
    pub(crate) fn next_item(&mut self) -> DbResult<Option<T>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).map_err(spill_io)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        // Guard against corrupted spill files that declare unreasonable item sizes.
        const MAX_SPILL_ITEM_BYTES: usize = 256 * 1024 * 1024; // 256 MiB
        if len > MAX_SPILL_ITEM_BYTES {
            return Err(DbError::internal(format!(
                "spill file item too large ({len} bytes, max {MAX_SPILL_ITEM_BYTES})"
            )));
        }
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).map_err(spill_io)?;
        let item: T = bincode::deserialize(&buf)
            .map_err(|e| DbError::internal(format!("spill decode: {e}")))?;
        self.remaining -= 1;
        Ok(Some(item))
    }
}

impl<T> Drop for SpillReader<T> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn spill_io(e: io::Error) -> DbError {
    DbError::internal(format!("spill I/O error: {e}"))
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::Value;

    fn test_spill_dir(tag: &str) -> PathBuf {
        let seq = SPILL_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aiondb_spill_test_{tag}_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn make_row(vals: Vec<Value>) -> Row {
        Row::new(vals)
    }

    #[test]
    fn roundtrip_single_row() {
        let dir = test_spill_dir("single");

        let row = make_row(vec![Value::Int(42), Value::Text("hello".into())]);
        let mut writer: SpillWriter<Row> = SpillWriter::new(&dir).unwrap();
        writer.write_item(&row).unwrap();
        assert_eq!(writer.row_count, 1);

        let mut reader = writer.into_reader().unwrap();
        let read_back = reader.next_item().unwrap().unwrap();
        assert_eq!(read_back.values[0], Value::Int(42));
        assert_eq!(read_back.values[1], Value::Text("hello".into()));
        assert!(reader.next_item().unwrap().is_none());
    }

    #[test]
    fn roundtrip_many_rows() {
        let dir = test_spill_dir("many");

        let mut writer: SpillWriter<Row> = SpillWriter::new(&dir).unwrap();
        for i in 0..1000 {
            writer
                .write_item(&make_row(vec![
                    Value::Int(i),
                    Value::BigInt(i64::from(i) * 100),
                ]))
                .unwrap();
        }
        assert_eq!(writer.row_count, 1000);

        let mut reader = writer.into_reader().unwrap();
        for i in 0..1000 {
            let row = reader.next_item().unwrap().unwrap();
            assert_eq!(row.values[0], Value::Int(i));
        }
        assert!(reader.next_item().unwrap().is_none());
    }

    #[test]
    fn roundtrip_null_and_diverse_types() {
        let dir = test_spill_dir("diverse");

        let row = make_row(vec![
            Value::Null,
            Value::Boolean(true),
            Value::Double(3.14),
            Value::Blob(vec![0xDE, 0xAD]),
        ]);

        let mut writer: SpillWriter<Row> = SpillWriter::new(&dir).unwrap();
        writer.write_item(&row).unwrap();
        let mut reader = writer.into_reader().unwrap();
        let read_back = reader.next_item().unwrap().unwrap();
        assert_eq!(read_back.values[0], Value::Null);
        assert_eq!(read_back.values[1], Value::Boolean(true));
        assert_eq!(read_back.values[3], Value::Blob(vec![0xDE, 0xAD]));
    }

    #[test]
    fn empty_spill_file() {
        let dir = test_spill_dir("empty");

        let writer: SpillWriter<Row> = SpillWriter::new(&dir).unwrap();
        assert_eq!(writer.row_count, 0);
        let mut reader = writer.into_reader().unwrap();
        assert!(reader.next_item().unwrap().is_none());
    }
}
