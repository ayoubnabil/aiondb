use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use aiondb_core::{DbError, DbResult};

const SSTABLE_MAGIC: [u8; 4] = [0x53, 0x53, 0x54, 0x42];
const SSTABLE_VERSION: u16 = 1;
#[cfg(test)]
const DEFAULT_BLOCK_SIZE: usize = 4096;
const MAX_SSTABLE_BLOCK_COUNT: usize = 1_000_000;
const MAX_SSTABLE_VALUE_BYTES: usize = 16 * 1024 * 1024;

enum EncodedSstableValue<'a> {
    Present { bytes: &'a [u8], len: u32 },
    Tombstone,
}

pub(crate) struct SSTableWriter {
    path: PathBuf,
    temp_path: PathBuf,
    writer: Option<BufWriter<File>>,
    block_size: usize,
    index_entries: Vec<(Vec<u8>, u64)>,
    current_block: Vec<u8>,
    current_block_count: u16,
    current_block_first_key: Option<Vec<u8>>,
    block_offset: u64,
    finished: bool,
}

impl SSTableWriter {
    #[cfg(test)]
    pub(crate) fn create(path: &Path) -> DbResult<Self> {
        Self::create_with_block_size(path, DEFAULT_BLOCK_SIZE)
    }

    pub(crate) fn create_with_block_size(path: &Path, block_size: usize) -> DbResult<Self> {
        let temp_path = sstable_temp_path(path);
        if temp_path.exists() {
            std::fs::remove_file(&temp_path).map_err(|error| {
                DbError::internal(format!(
                    "sstable clear stale temp '{}': {error}",
                    temp_path.display()
                ))
            })?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|error| {
                DbError::internal(format!("sstable create '{}': {error}", temp_path.display()))
            })?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&SSTABLE_MAGIC)
            .map_err(|error| DbError::internal(format!("sstable header: {error}")))?;
        writer
            .write_all(&SSTABLE_VERSION.to_le_bytes())
            .map_err(|error| DbError::internal(format!("sstable version: {error}")))?;
        writer
            .write_all(&0u32.to_le_bytes())
            .map_err(|error| DbError::internal(format!("sstable block_count: {error}")))?;

        Ok(Self {
            path: path.to_path_buf(),
            temp_path,
            writer: Some(writer),
            block_size,
            index_entries: Vec::new(),
            current_block: Vec::new(),
            current_block_count: 0,
            current_block_first_key: None,
            block_offset: 10,
            finished: false,
        })
    }

    pub(crate) fn add(&mut self, key: &[u8], value: Option<&[u8]>) -> DbResult<()> {
        let key_len = u16::try_from(key.len()).map_err(|_| {
            DbError::internal(format!(
                "sstable key too large for u16 length field: {} bytes",
                key.len()
            ))
        })?;
        let encoded_value = match value {
            Some(value) => {
                if value.len() > MAX_SSTABLE_VALUE_BYTES {
                    return Err(DbError::internal(format!(
                        "sstable value too large: {} bytes exceeds maximum of {MAX_SSTABLE_VALUE_BYTES}",
                        value.len()
                    )));
                }
                let len = u32::try_from(value.len()).map_err(|_| {
                    DbError::internal(format!(
                        "sstable value too large for u32 length field: {} bytes",
                        value.len()
                    ))
                })?;
                EncodedSstableValue::Present { bytes: value, len }
            }
            None => EncodedSstableValue::Tombstone,
        };

        if self.current_block_first_key.is_none() {
            self.current_block_first_key = Some(key.to_vec());
        }

        self.current_block.extend_from_slice(&key_len.to_le_bytes());
        self.current_block.extend_from_slice(key);

        match encoded_value {
            EncodedSstableValue::Present { bytes, len } => {
                self.current_block.extend_from_slice(&len.to_le_bytes());
                self.current_block.extend_from_slice(bytes);
            }
            EncodedSstableValue::Tombstone => self
                .current_block
                .extend_from_slice(&u32::MAX.to_le_bytes()),
        }

        self.current_block_count = self.current_block_count.checked_add(1).ok_or_else(|| {
            DbError::internal("sstable block entry count overflowed u16".to_string())
        })?;
        if self.current_block.len() >= self.block_size {
            self.flush_block()?;
        }

        Ok(())
    }

    fn flush_block(&mut self) -> DbResult<()> {
        if self.current_block_count == 0 {
            return Ok(());
        }

        let first_key = self.current_block_first_key.take().ok_or_else(|| {
            DbError::internal(
                "sstable writer invariant violated: missing first key for non-empty block",
            )
        })?;

        self.writer
            .as_mut()
            .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
            .write_all(&self.current_block_count.to_le_bytes())
            .map_err(|error| DbError::internal(format!("sstable block header: {error}")))?;
        self.writer
            .as_mut()
            .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
            .write_all(&self.current_block)
            .map_err(|error| DbError::internal(format!("sstable block data: {error}")))?;

        let block_start = self.block_offset;
        let block_len = u64::try_from(self.current_block.len()).map_err(|_| {
            DbError::internal(format!(
                "sstable block too large for u64 offset accounting: {} bytes",
                self.current_block.len()
            ))
        })?;
        self.block_offset = self
            .block_offset
            .checked_add(2)
            .and_then(|offset| offset.checked_add(block_len))
            .ok_or_else(|| DbError::internal("sstable block offset overflow"))?;
        self.index_entries.push((first_key, block_start));
        self.current_block.clear();
        self.current_block_count = 0;

        Ok(())
    }

    pub(crate) fn finish(mut self) -> DbResult<()> {
        self.flush_block()?;

        let index_offset = self.block_offset;
        let block_count = u32::try_from(self.index_entries.len()).map_err(|_| {
            DbError::internal(format!(
                "sstable block count exceeds u32: {}",
                self.index_entries.len()
            ))
        })?;

        for (first_key, offset) in &self.index_entries {
            let key_len = u16::try_from(first_key.len()).map_err(|_| {
                DbError::internal(format!(
                    "sstable index key too large for u16 length field: {} bytes",
                    first_key.len()
                ))
            })?;
            self.writer
                .as_mut()
                .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
                .write_all(&key_len.to_le_bytes())
                .map_err(|error| DbError::internal(format!("sstable index key: {error}")))?;
            self.writer
                .as_mut()
                .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
                .write_all(first_key)
                .map_err(|error| DbError::internal(format!("sstable index key data: {error}")))?;
            self.writer
                .as_mut()
                .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
                .write_all(&offset.to_le_bytes())
                .map_err(|error| DbError::internal(format!("sstable index offset: {error}")))?;
        }

        self.writer
            .as_mut()
            .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
            .write_all(&index_offset.to_le_bytes())
            .map_err(|error| DbError::internal(format!("sstable index_offset: {error}")))?;
        self.writer
            .as_mut()
            .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
            .flush()
            .map_err(|error| DbError::internal(format!("sstable flush: {error}")))?;
        let mut inner = self
            .writer
            .take()
            .ok_or_else(|| DbError::internal("sstable writer is already finished"))?
            .into_inner()
            .map_err(|error| DbError::internal(format!("sstable into_inner: {error}")))?;
        inner
            .seek(SeekFrom::Start(6))
            .map_err(|error| DbError::internal(format!("sstable seek header: {error}")))?;
        inner
            .write_all(&block_count.to_le_bytes())
            .map_err(|error| DbError::internal(format!("sstable write block_count: {error}")))?;
        inner
            .sync_all()
            .map_err(|error| DbError::internal(format!("sstable sync: {error}")))?;
        drop(inner);
        std::fs::rename(&self.temp_path, &self.path).map_err(|error| {
            DbError::internal(format!(
                "sstable publish '{}' -> '{}': {error}",
                self.temp_path.display(),
                self.path.display()
            ))
        })?;
        sync_parent_dir(&self.path)?;
        self.finished = true;

        Ok(())
    }
}

impl Drop for SSTableWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}

#[derive(Debug, Clone)]
struct BlockIndex {
    first_key: Vec<u8>,
    offset: u64,
}

pub(crate) struct SSTableReader {
    file: File,
    index: Vec<BlockIndex>,
}

impl SSTableReader {
    pub(crate) fn open(path: &Path) -> DbResult<Self> {
        let mut file = File::open(path).map_err(|error| {
            DbError::internal(format!("sstable open '{}': {error}", path.display()))
        })?;
        let file_len = file
            .metadata()
            .map_err(|error| DbError::internal(format!("sstable metadata: {error}")))?
            .len();
        if file_len < 18 {
            return Err(DbError::internal(format!(
                "sstable '{}' is truncated ({} bytes)",
                path.display(),
                file_len
            )));
        }

        let mut header = [0u8; 10];
        file.read_exact(&mut header)
            .map_err(|error| DbError::internal(format!("sstable header read: {error}")))?;
        if header[0..4] != SSTABLE_MAGIC {
            return Err(DbError::internal(format!(
                "invalid sstable magic in '{}'",
                path.display()
            )));
        }
        let version = u16::from_le_bytes([header[4], header[5]]);
        if version != SSTABLE_VERSION {
            return Err(DbError::feature_not_supported(format!(
                "unsupported sstable version {version} in '{}'",
                path.display()
            )));
        }
        let block_count = u32::from_le_bytes([header[6], header[7], header[8], header[9]]);
        let block_count = usize::try_from(block_count)
            .map_err(|_| DbError::internal("sstable block_count overflows usize"))?;
        if block_count > MAX_SSTABLE_BLOCK_COUNT {
            return Err(DbError::internal(format!(
                "sstable block_count {block_count} exceeds limit {MAX_SSTABLE_BLOCK_COUNT}"
            )));
        }

        file.seek(SeekFrom::End(-8))
            .map_err(|error| DbError::internal(format!("sstable seek index_offset: {error}")))?;
        let mut buf8 = [0u8; 8];
        file.read_exact(&mut buf8)
            .map_err(|error| DbError::internal(format!("sstable read index_offset: {error}")))?;
        let index_offset = u64::from_le_bytes(buf8);
        if index_offset < 10 || index_offset > file_len.saturating_sub(8) {
            return Err(DbError::internal(format!(
                "sstable index offset {index_offset} is out of bounds for file length {file_len}"
            )));
        }
        let available_index_bytes = file_len.saturating_sub(8).saturating_sub(index_offset);
        let min_index_entry_bytes = 10u64; // u16 key_len + u64 block_offset
        if u64::try_from(block_count).unwrap_or(u64::MAX)
            > available_index_bytes / min_index_entry_bytes
        {
            return Err(DbError::internal(format!(
                "sstable index metadata is truncated: block_count {block_count} cannot fit in {available_index_bytes} bytes"
            )));
        }

        file.seek(SeekFrom::Start(index_offset))
            .map_err(|error| DbError::internal(format!("sstable seek index: {error}")))?;
        let mut index = Vec::with_capacity(block_count);
        // Defensive cap to prevent a malicious sstable file from forcing GiB-
        // scale index-key allocation. Without this, MAX_SSTABLE_BLOCK_COUNT
        // (1M) × u16::MAX per-key bytes (~64 KB) = ~64 GiB heap.
        const MAX_SSTABLE_INDEX_KEY_BYTES: usize = 4096;
        let mut total_index_key_bytes: usize = 0;
        const MAX_SSTABLE_TOTAL_INDEX_KEY_BYTES: usize = 256 * 1024 * 1024;
        for _ in 0..block_count {
            let mut key_len = [0u8; 2];
            file.read_exact(&mut key_len)
                .map_err(|error| DbError::internal(format!("sstable index klen: {error}")))?;
            let key_len = usize::from(u16::from_le_bytes(key_len));
            if key_len > MAX_SSTABLE_INDEX_KEY_BYTES {
                return Err(DbError::internal(format!(
                    "sstable index key length {key_len} exceeds limit {MAX_SSTABLE_INDEX_KEY_BYTES}"
                )));
            }
            total_index_key_bytes = total_index_key_bytes.saturating_add(key_len);
            if total_index_key_bytes > MAX_SSTABLE_TOTAL_INDEX_KEY_BYTES {
                return Err(DbError::internal(format!(
                    "sstable cumulative index key bytes exceed limit {MAX_SSTABLE_TOTAL_INDEX_KEY_BYTES}"
                )));
            }
            let mut first_key = vec![0u8; key_len];
            file.read_exact(&mut first_key)
                .map_err(|error| DbError::internal(format!("sstable index key: {error}")))?;
            file.read_exact(&mut buf8)
                .map_err(|error| DbError::internal(format!("sstable index offset: {error}")))?;
            index.push(BlockIndex {
                first_key,
                offset: u64::from_le_bytes(buf8),
            });
        }

        Ok(Self { file, index })
    }

    #[allow(clippy::option_option)]
    pub(crate) fn get(&self, key: &[u8]) -> DbResult<Option<Option<Vec<u8>>>> {
        if self.index.is_empty() {
            return Ok(None);
        }

        let block_index = self
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= key);
        if block_index == 0 {
            return Ok(None);
        }

        self.search_block(block_index - 1, key)
    }

    #[allow(clippy::option_option)]
    fn search_block(&self, block_index: usize, key: &[u8]) -> DbResult<Option<Option<Vec<u8>>>> {
        let mut cursor = self.index[block_index].offset;
        let mut entry_key = Vec::new();

        let mut count = [0u8; 2];
        self.read_exact_at(cursor, &mut count, "block count")?;
        cursor += 2;
        let entry_count = u16::from_le_bytes(count);

        for _ in 0..entry_count {
            let mut key_len = [0u8; 2];
            self.read_exact_at(cursor, &mut key_len, "entry klen")?;
            cursor += 2;
            let key_len = usize::from(u16::from_le_bytes(key_len));
            entry_key.resize(key_len, 0);
            self.read_exact_at(cursor, entry_key.as_mut_slice(), "entry key")?;
            cursor += u64::try_from(key_len).unwrap_or(0);

            let mut value_len = [0u8; 4];
            self.read_exact_at(cursor, &mut value_len, "entry vlen")?;
            cursor += 4;
            let value_len = u32::from_le_bytes(value_len);

            if entry_key.as_slice() == key {
                if value_len == u32::MAX {
                    return Ok(Some(None));
                }
                if usize::try_from(value_len).unwrap_or(usize::MAX) > MAX_SSTABLE_VALUE_BYTES {
                    return Err(DbError::internal(format!(
                        "sstable value length {value_len} exceeds maximum {MAX_SSTABLE_VALUE_BYTES} bytes"
                    )));
                }

                let value_len = usize::try_from(value_len)
                    .map_err(|_| DbError::internal("sstable value length overflow"))?;
                let mut value = vec![0u8; value_len];
                self.read_exact_at(cursor, &mut value, "entry value")?;
                return Ok(Some(Some(value)));
            }

            if value_len != u32::MAX {
                cursor = cursor.saturating_add(u64::from(value_len));
            }
            if entry_key.as_slice() > key {
                break;
            }
        }

        Ok(None)
    }

    #[allow(clippy::iter_not_returning_iterator)]
    pub(crate) fn iter(&self) -> DbResult<Vec<(Vec<u8>, Option<Vec<u8>>)>> {
        let mut entries = Vec::new();
        for block in &self.index {
            let mut cursor = block.offset;

            let mut count = [0u8; 2];
            self.read_exact_at(cursor, &mut count, "iter count")?;
            cursor += 2;
            let entry_count = u16::from_le_bytes(count);

            for _ in 0..entry_count {
                let mut key_len = [0u8; 2];
                self.read_exact_at(cursor, &mut key_len, "iter klen")?;
                cursor += 2;
                let key_len = usize::from(u16::from_le_bytes(key_len));
                let mut key = vec![0u8; key_len];
                self.read_exact_at(cursor, &mut key, "iter key")?;
                cursor += u64::try_from(key_len).unwrap_or(0);

                let mut value_len = [0u8; 4];
                self.read_exact_at(cursor, &mut value_len, "iter vlen")?;
                cursor += 4;
                let value_len = u32::from_le_bytes(value_len);
                if value_len == u32::MAX {
                    entries.push((key, None));
                    continue;
                }
                if usize::try_from(value_len).unwrap_or(usize::MAX) > MAX_SSTABLE_VALUE_BYTES {
                    return Err(DbError::internal(format!(
                        "sstable value length {value_len} exceeds maximum {MAX_SSTABLE_VALUE_BYTES} bytes"
                    )));
                }

                let value_len = usize::try_from(value_len)
                    .map_err(|_| DbError::internal("sstable value length overflow"))?;
                let mut value = vec![0u8; value_len];
                self.read_exact_at(cursor, &mut value, "iter value")?;
                cursor += u64::try_from(value_len).unwrap_or(0);
                entries.push((key, Some(value)));
            }
        }

        Ok(entries)
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8], context: &str) -> DbResult<()> {
        read_exact_at(&self.file, offset, buffer)
            .map_err(|error| DbError::internal(format!("sstable {context}: {error}")))
    }
}

fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let read = read_at(file, buffer, offset)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF while reading sstable",
            ));
        }
        offset = offset.saturating_add(u64::try_from(read).unwrap_or(0));
        let (_, rest) = buffer.split_at_mut(read);
        buffer = rest;
    }
    Ok(())
}

fn sstable_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sstable");
    path.with_file_name(format!(".{file_name}.tmp"))
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "sstable sync directory '{}': {error}",
            parent.display()
        ))
    })
}

#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut handle = file.try_clone()?;
    handle.seek(SeekFrom::Start(offset))?;
    handle.read(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;
    use std::collections::BTreeMap;

    #[test]
    fn sstable_writer_reader_roundtrip() {
        let path = unique_temp_path("lsm-sstable", "roundtrip").with_extension("sst");
        let mut writer = SSTableWriter::create(&path).expect("sstable writer should open");
        let mut entries = BTreeMap::new();
        entries.insert("backend", b"lsm".to_vec());
        entries.insert("checkpoint_lsn", 42u64.to_le_bytes().to_vec());
        entries.insert("dirty_pages_flushed", 7u64.to_le_bytes().to_vec());
        entries.insert("kind", b"checkpoint_segment_v1".to_vec());
        entries.insert("level", 0u32.to_le_bytes().to_vec());
        entries.insert("version", 1u64.to_le_bytes().to_vec());

        for (key, value) in &entries {
            writer
                .add(key.as_bytes(), Some(value.as_slice()))
                .expect("sstable entry should be writable");
        }
        writer.finish().expect("sstable should finish");

        let reader = SSTableReader::open(&path).expect("sstable reader should open");
        for (key, value) in &entries {
            assert_eq!(
                reader
                    .get(key.as_bytes())
                    .expect("sstable lookup should succeed"),
                Some(Some(value.clone()))
            );
        }
        assert_eq!(
            reader
                .get(b"missing")
                .expect("missing lookup should still succeed"),
            None
        );
        assert_eq!(
            reader
                .iter()
                .expect("sstable iteration should succeed")
                .len(),
            entries.len()
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sstable_writer_only_exposes_final_path_after_finish() {
        let path = unique_temp_path("lsm-sstable", "atomic-publish").with_extension("sst");
        let temp_path = sstable_temp_path(&path);
        let mut writer = SSTableWriter::create(&path).expect("sstable writer should open");
        writer
            .add(b"k", Some(b"value"))
            .expect("sstable entry should be writable");

        assert!(
            !path.exists(),
            "final sstable path must stay hidden until finish publishes it"
        );
        assert!(
            temp_path.exists(),
            "temp sstable path should exist before finish"
        );

        writer.finish().expect("sstable should finish");

        assert!(path.is_file(), "final sstable path should be published");
        assert!(
            !temp_path.exists(),
            "temp sstable path should be removed after publish"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sstable_writer_drop_cleans_up_unpublished_temp_file() {
        let path = unique_temp_path("lsm-sstable", "drop-cleans-temp").with_extension("sst");
        let temp_path = sstable_temp_path(&path);
        {
            let mut writer = SSTableWriter::create(&path).expect("sstable writer should open");
            writer
                .add(b"k", Some(b"value"))
                .expect("sstable entry should be writable");
            assert!(temp_path.exists(), "temp path should exist while writing");
        }

        assert!(
            !path.exists(),
            "dropping an unfinished writer must not expose a final sstable"
        );
        assert!(
            !temp_path.exists(),
            "dropping an unfinished writer should clean up the temp sstable"
        );
    }

    #[test]
    fn sstable_writer_rejects_values_reader_would_refuse() {
        let path = unique_temp_path("lsm-sstable", "writer-oversized-value").with_extension("sst");
        let mut writer = SSTableWriter::create(&path).expect("sstable writer should open");
        let oversized = vec![0u8; MAX_SSTABLE_VALUE_BYTES + 1];

        let error = writer
            .add(b"too-large", Some(&oversized))
            .expect_err("writer should reject oversized value before mutating the block");
        assert!(
            format!("{error}").contains("exceeds maximum"),
            "unexpected error: {error}"
        );
        writer
            .add(b"ok", Some(b"value"))
            .expect("writer should remain usable after rejected value");
        writer.finish().expect("sstable should finish");

        let reader = SSTableReader::open(&path).expect("sstable reader should open");
        assert_eq!(
            reader.get(b"ok").expect("lookup should succeed"),
            Some(Some(b"value".to_vec()))
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sstable_reader_rejects_excessive_block_count_header() {
        let path = unique_temp_path("lsm-sstable", "excessive-block-count").with_extension("sst");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SSTABLE_MAGIC);
        bytes.extend_from_slice(&SSTABLE_VERSION.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(MAX_SSTABLE_BLOCK_COUNT + 1)
                .expect("block count bound should fit u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&10u64.to_le_bytes());
        std::fs::write(&path, bytes).expect("write corrupted sstable");

        let Err(error) = SSTableReader::open(&path) else {
            panic!("open should reject oversized block_count")
        };
        assert!(
            format!("{error}").contains("block_count"),
            "unexpected error: {error}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sstable_reader_get_rejects_oversized_value_length() {
        let path = unique_temp_path("lsm-sstable", "oversized-value-length").with_extension("sst");
        let oversized_len =
            u32::try_from(MAX_SSTABLE_VALUE_BYTES + 1).expect("value-size bound should fit u32");

        // Header
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SSTABLE_MAGIC);
        bytes.extend_from_slice(&SSTABLE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // block_count

        // One data block at offset 10 with one key/value entry.
        bytes.extend_from_slice(&1u16.to_le_bytes()); // entry_count
        bytes.extend_from_slice(&1u16.to_le_bytes()); // key_len
        bytes.push(b'k');
        bytes.extend_from_slice(&oversized_len.to_le_bytes()); // oversized value len

        // Index section starts immediately after block bytes.
        let index_offset = 10u64 + 2 + 2 + 1 + 4;
        bytes.extend_from_slice(&1u16.to_le_bytes()); // first_key len
        bytes.push(b'k'); // first_key
        bytes.extend_from_slice(&10u64.to_le_bytes()); // block offset

        // Trailer: index offset
        bytes.extend_from_slice(&index_offset.to_le_bytes());
        std::fs::write(&path, bytes).expect("write crafted sstable");

        let reader = SSTableReader::open(&path).expect("reader open should succeed");
        let error = reader
            .get(b"k")
            .expect_err("lookup should reject oversized value length");
        assert!(
            format!("{error}").contains("exceeds maximum"),
            "unexpected error: {error}"
        );

        let _ = std::fs::remove_file(path);
    }
}
