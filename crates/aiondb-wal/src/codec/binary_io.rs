use aiondb_core::{DbError, DbResult};

// ---------------------------------------------------------------------------
// BinaryWriter
// ---------------------------------------------------------------------------

pub(crate) struct BinaryWriter {
    buf: Vec<u8>,
}

impl BinaryWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i128(&mut self, v: i128) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    pub fn write_raw(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> DbResult<()> {
        let len: u32 = data.len().try_into().map_err(|e| {
            DbError::internal(format!(
                "write_bytes: data length {} exceeds u32::MAX: {e}",
                data.len()
            ))
        })?;
        self.write_u32(len);
        self.buf.extend_from_slice(data);
        Ok(())
    }

    pub fn write_str(&mut self, s: &str) -> DbResult<()> {
        self.write_bytes(s.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// BinaryReader
// ---------------------------------------------------------------------------

pub(crate) struct BinaryReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn ensure(&self, n: usize) -> DbResult<()> {
        match self.pos.checked_add(n) {
            Some(end) if end <= self.data.len() => Ok(()),
            _ => Err(DbError::internal("WAL record truncated")),
        }
    }

    fn read_array<const N: usize>(&mut self) -> DbResult<[u8; N]> {
        self.ensure(N)?;
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(&self.data[self.pos..self.pos + N]);
        self.pos += N;
        Ok(bytes)
    }

    pub fn read_u8(&mut self) -> DbResult<u8> {
        self.ensure(1)?;
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    pub fn read_u16(&mut self) -> DbResult<u16> {
        self.read_array().map(u16::from_le_bytes)
    }

    pub fn read_u32(&mut self) -> DbResult<u32> {
        self.read_array().map(u32::from_le_bytes)
    }

    pub fn read_u64(&mut self) -> DbResult<u64> {
        self.read_array().map(u64::from_le_bytes)
    }

    pub fn read_i32(&mut self) -> DbResult<i32> {
        self.read_array().map(i32::from_le_bytes)
    }

    pub fn read_i64(&mut self) -> DbResult<i64> {
        self.read_array().map(i64::from_le_bytes)
    }

    pub fn read_i128(&mut self) -> DbResult<i128> {
        self.read_array().map(i128::from_le_bytes)
    }

    pub fn read_f32(&mut self) -> DbResult<f32> {
        self.read_array().map(f32::from_le_bytes)
    }

    pub fn read_f64(&mut self) -> DbResult<f64> {
        self.read_array().map(f64::from_le_bytes)
    }

    pub fn read_bool(&mut self) -> DbResult<bool> {
        let v = self.read_u8()?;
        match v {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DbError::internal(format!("WAL: invalid boolean tag {v}"))),
        }
    }

    /// Maximum single allocation when decoding a WAL record (64 MiB).
    /// This prevents a corrupted or malicious length field from triggering
    /// a multi-gigabyte allocation that could OOM-crash the process.
    const MAX_READ_ALLOC: usize = 64 * 1024 * 1024;

    pub fn read_bytes(&mut self) -> DbResult<Vec<u8>> {
        let len = usize::try_from(self.read_u32()?).map_err(|e| {
            DbError::internal(format!(
                "WAL: invalid length conversion from u32 to usize: {e}"
            ))
        })?;
        if len > Self::MAX_READ_ALLOC {
            return Err(DbError::internal(format!(
                "WAL: allocation size {len} exceeds limit ({})",
                Self::MAX_READ_ALLOC,
            )));
        }
        self.ensure(len)?;
        let v = self.data[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(v)
    }

    pub fn read_str(&mut self) -> DbResult<String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes)
            .map_err(|e| DbError::internal(format!("WAL: invalid UTF-8 in string: {e}")))
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Return a capacity hint that is bounded by the remaining bytes in the
    /// buffer. This prevents a corrupted length field from triggering an
    /// oversized allocation that could OOM-crash the process.
    pub fn capped_capacity(&self, requested: usize) -> usize {
        requested.min(self.remaining())
    }
}
