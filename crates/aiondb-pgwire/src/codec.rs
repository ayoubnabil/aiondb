//! Binary frame encoding/decoding for the `PostgreSQL` v3 wire protocol.
//!
//! # Wire format
//! - Startup message: `length(u32) | protocol_version(u32) | key\0value\0...\0`
//! - Frontend messages: `tag(u8) | length(u32, includes self) | payload`
//! - Backend messages: `tag(u8) | length(u32, includes self) | payload`
//!
//! All multi-byte integers are big-endian.

use std::collections::BTreeMap;

use aiondb_core::DbError;
use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum allowed message size (8 MiB). Prevents `DoS` via oversized
/// allocation from untrusted length fields. With 128 max connections, the
/// worst-case memory footprint from in-flight message buffers is 1 GiB.
pub const MAX_MESSAGE_SIZE: usize = 8 * 1024 * 1024;
const MAX_STARTUP_PARAMS: usize = 32;
const MAX_STARTUP_PARAM_NAME_LEN: usize = 64;
const MAX_STARTUP_PARAM_LEN: usize = 256;
const MAX_STARTUP_OPTIONS_LEN: usize = 1024;

/// Protocol version 3.0 = 196608 (`0x0003_0000`).
pub const PROTOCOL_V3: u32 = 196_608;

/// SSL request magic number.
pub const SSL_REQUEST: u32 = 80_877_103;

/// Cancel request magic number.
pub const CANCEL_REQUEST: u32 = 80_877_102;

/// A raw startup message read from the client.
#[derive(Debug)]
pub enum StartupPayload {
    /// Normal v3 startup with key-value parameters.
    Startup(BTreeMap<String, String>),
    /// Client sent an `SSLRequest`. Normally handled during TLS negotiation
    /// before `read_startup` is called; this variant is reached only if the
    /// startup bytes are replayed after negotiation.
    SslRequest,
    /// Cancel request with (`pid`, `secret_key`).
    CancelRequest(u32, u32),
}

/// A raw frontend message (tag + payload bytes).
#[derive(Debug)]
pub struct RawFrontendMessage {
    pub tag: u8,
    pub payload: BytesMut,
}

/// Read the initial startup message from the client.
///
/// The startup message has no tag byte: `[length: u32][payload]`.
pub async fn read_startup<R: AsyncRead + Unpin>(reader: &mut R) -> Result<StartupPayload, DbError> {
    let len = read_i32(reader).await?;
    let payload_len = validate_payload_len(len, 8, "startup message too short", "startup message")?;
    let mut buf = read_payload(reader, payload_len, "read startup payload").await?;

    let version = buf.get_u32();

    if version == SSL_REQUEST {
        if buf.has_remaining() {
            return Err(DbError::protocol(
                "SSL request contains unexpected trailing bytes",
            ));
        }
        return Ok(StartupPayload::SslRequest);
    }
    if version == CANCEL_REQUEST {
        if buf.remaining() != 8 {
            return Err(DbError::protocol(
                "cancel request must contain exactly 8 bytes of payload",
            ));
        }
        let pid = buf.get_u32();
        let key = buf.get_u32();
        return Ok(StartupPayload::CancelRequest(pid, key));
    }
    if version != PROTOCOL_V3 {
        return Err(DbError::protocol(format!(
            "unsupported protocol version: {version:#010x}"
        )));
    }

    // Parse null-terminated key=value pairs.
    let mut params = BTreeMap::new();
    let mut saw_terminator = false;
    while buf.has_remaining() {
        if params.len() >= MAX_STARTUP_PARAMS {
            return Err(DbError::protocol(format!(
                "too many startup parameters ({}, maximum is {MAX_STARTUP_PARAMS})",
                params.len().saturating_add(1)
            )));
        }
        let key = read_cstring_from_buf_with_limit(
            &mut buf,
            MAX_STARTUP_PARAM_NAME_LEN,
            "startup parameter name",
        )?;
        if key.is_empty() {
            if buf.has_remaining() {
                return Err(DbError::protocol(
                    "startup params contain trailing bytes after terminator",
                ));
            }
            saw_terminator = true;
            break; // Trailing null terminator.
        }
        let max_len = if key == "options" {
            MAX_STARTUP_OPTIONS_LEN
        } else {
            MAX_STARTUP_PARAM_LEN
        };
        let value = read_cstring_from_buf_with_limit(
            &mut buf,
            max_len,
            &format!("startup parameter \"{key}\""),
        )?;
        if params.insert(key.clone(), value).is_some() {
            return Err(DbError::protocol(format!(
                "startup parameter \"{key}\" is duplicated"
            )));
        }
    }
    if !saw_terminator {
        return Err(DbError::protocol(
            "startup params must end with a null terminator",
        ));
    }

    Ok(StartupPayload::Startup(params))
}

/// Read a single frontend message (after startup).
///
/// Format: `[tag: u8][length: u32 (includes self)][payload]`.
pub async fn read_frontend_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<RawFrontendMessage, DbError> {
    let mut tag_buf = [0u8; 1];
    reader
        .read_exact(&mut tag_buf)
        .await
        .map_err(|e| DbError::protocol(format!("read tag: {e}")))?;
    let tag = tag_buf[0];

    let len = read_i32(reader).await?;
    let payload_len = validate_payload_len(len, 4, "message length too short", "frontend message")?;
    let payload = read_payload(reader, payload_len, "read payload").await?;

    Ok(RawFrontendMessage { tag, payload })
}

// ---------------------------------------------------------------------------
// Backend message writer
// ---------------------------------------------------------------------------

/// A buffer for building backend messages.
#[derive(Debug, Default)]
pub struct MessageWriter {
    buf: Vec<u8>,
}

impl MessageWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.buf.reserve(additional);
    }

    /// Begin a new message with the given tag byte.
    /// Returns the position of the length placeholder.
    pub fn begin(&mut self, tag: u8) -> usize {
        self.buf.push(tag);
        let pos = self.buf.len();
        self.buf.put_u32(0); // placeholder for length
        pos
    }

    /// Finish a message started with `begin`, filling in the length.
    pub fn finish(&mut self, len_pos: usize) {
        let msg_len = u32::try_from(self.buf.len().saturating_sub(len_pos)).unwrap_or(u32::MAX);
        self.buf[len_pos..len_pos + 4].copy_from_slice(&msg_len.to_be_bytes());
    }

    /// Finish a message and reject frames that exceed the negotiated safety
    /// cap instead of silently saturating the length field.
    pub fn try_finish(&mut self, len_pos: usize) -> Result<(), DbError> {
        let len_end = len_pos
            .checked_add(4)
            .ok_or_else(|| DbError::internal("pgwire message length position overflow"))?;
        if len_end > self.buf.len() {
            return Err(DbError::internal("invalid pgwire message length position"));
        }
        let msg_len = self
            .buf
            .len()
            .checked_sub(len_pos)
            .ok_or_else(|| DbError::internal("pgwire message length underflow"))?;
        let payload_len = msg_len
            .checked_sub(4)
            .ok_or_else(|| DbError::internal("pgwire message payload length underflow"))?;
        if payload_len > MAX_MESSAGE_SIZE {
            return Err(DbError::internal(format!(
                "pgwire backend message too large ({payload_len} bytes, max {MAX_MESSAGE_SIZE})"
            )));
        }
        let msg_len = u32::try_from(msg_len).map_err(|_| {
            DbError::internal(format!(
                "pgwire backend message length too large ({msg_len})"
            ))
        })?;
        self.buf[len_pos..len_pos + 4].copy_from_slice(&msg_len.to_be_bytes());
        Ok(())
    }

    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn put_i16(&mut self, v: i16) {
        self.buf.put_i16(v);
    }

    pub fn put_u16(&mut self, v: u16) {
        self.buf.put_u16(v);
    }

    pub fn put_i32(&mut self, v: i32) {
        self.buf.put_i32(v);
    }

    pub fn put_u32(&mut self, v: u32) {
        self.buf.put_u32(v);
    }

    /// Write a null-terminated string.
    ///
    /// Embedded NUL bytes would corrupt the pgwire frame layout because
    /// backend strings are C-strings on the wire. Replace them with spaces
    /// so responses remain protocol-valid even if upstream text is malformed.
    pub fn put_cstring(&mut self, s: &str) {
        let bytes = s.as_bytes();
        // Fast path: the overwhelming majority of cstrings the server
        // emits (field names, status keys, command tags, …) are short
        // ASCII without embedded NULs. `memchr` is a vectorised search
        // that bails out the moment it sees a NUL, so when none is
        // found we can `extend_from_slice` the whole string in one go
        // instead of pushing byte-by-byte through a per-byte branch.
        if !bytes.contains(&0) {
            self.buf.extend_from_slice(bytes);
            self.buf.push(0);
            return;
        }
        // Slow path: there's at least one embedded NUL. Sanitise it to
        // a space so the frame layout stays valid.
        for &byte in bytes {
            self.buf.push(if byte == 0 { b' ' } else { byte });
        }
        self.buf.push(0);
    }

    /// Write raw bytes.
    pub fn put_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Consume the writer and return the byte buffer.
    pub fn finish_message(self) -> Vec<u8> {
        self.buf
    }

    /// Write the buffer to an async writer and reset.
    pub async fn flush<W: AsyncWrite + Unpin>(&mut self, writer: &mut W) -> Result<(), DbError> {
        writer
            .write_all(&self.buf)
            .await
            .map_err(|e| DbError::protocol(format!("write: {e}")))?;
        self.buf.clear();
        Ok(())
    }

    /// Returns the current buffer length.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Provide mutable access to the underlying byte buffer.
    ///
    /// This allows direct-write patterns where a caller encodes data
    /// straight into the message buffer (e.g. `write_data_row_direct`).
    pub fn buf_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_payload_len(
    len: i32,
    minimum_len: i32,
    too_short_message: &str,
    message_kind: &str,
) -> Result<usize, DbError> {
    if len < minimum_len {
        return Err(DbError::protocol(too_short_message));
    }
    let payload_len_i32 = len
        .checked_sub(4)
        .ok_or_else(|| DbError::protocol(format!("{message_kind} length underflow")))?;
    let payload_len = usize::try_from(payload_len_i32).map_err(|_| {
        DbError::protocol(format!(
            "{message_kind} has invalid negative payload length ({payload_len_i32})"
        ))
    })?;
    if payload_len > MAX_MESSAGE_SIZE {
        return Err(DbError::protocol(format!(
            "{message_kind} too large ({payload_len} bytes, max {MAX_MESSAGE_SIZE})"
        )));
    }
    Ok(payload_len)
}

async fn read_payload<R: AsyncRead + Unpin>(
    reader: &mut R,
    payload_len: usize,
    read_context: &str,
) -> Result<BytesMut, DbError> {
    // Reserve `payload_len` bytes of capacity but leave them
    // uninitialised: `read_buf` writes directly into the unfilled
    // portion through tokio's `BufMut` interface, so we can skip the
    // `BytesMut::zeroed(payload_len)` memset that the previous
    // implementation paid on every frontend message --- including the
    // tiny per-iteration Sync / Bind / Execute payloads on the OLTP
    // hot loop. No unsafe is needed because `read_buf` only advances
    // `BytesMut::len()` once the bytes are actually written.
    let mut payload = BytesMut::with_capacity(payload_len);
    while payload.len() < payload_len {
        let read = reader
            .read_buf(&mut payload)
            .await
            .map_err(|e| DbError::protocol(format!("{read_context}: {e}")))?;
        if read == 0 {
            return Err(DbError::protocol(format!(
                "{read_context}: unexpected EOF after {} of {} bytes",
                payload.len(),
                payload_len
            )));
        }
    }
    Ok(payload)
}

async fn read_i32<R: AsyncRead + Unpin>(reader: &mut R) -> Result<i32, DbError> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .await
        .map_err(|e| DbError::protocol(format!("read i32: {e}")))?;
    Ok(i32::from_be_bytes(buf))
}

/// Read a null-terminated string from a `BytesMut`.
///
/// Validates UTF-8 on the borrowed slice *before* allocating the owned
/// `String`, so malformed input never triggers an allocation.
pub fn read_cstring_from_buf(buf: &mut BytesMut) -> Result<String, DbError> {
    read_cstring_from_buf_with_limit(buf, usize::MAX, "cstring")
}

pub fn read_cstring_from_buf_with_limit(
    buf: &mut BytesMut,
    max_len: usize,
    context: &str,
) -> Result<String, DbError> {
    let data = buf.as_ref();
    let nul = data
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| DbError::protocol(format!("missing null terminator in {context}")))?;
    if nul > max_len {
        return Err(DbError::protocol(format!(
            "{context} exceeds maximum length of {max_len} bytes"
        )));
    }
    // Validate UTF-8 on the borrowed slice before allocating an owned String
    // so malformed input is rejected without extra allocation work.
    let slice = &data[..nul];
    let s = std::str::from_utf8(slice)
        .map_err(|e| DbError::protocol(format!("invalid UTF-8: {e}")))?
        .to_owned();
    buf.advance(nul + 1);
    Ok(s)
}

/// Read a 16-bit signed integer from a `BytesMut`.
pub fn read_i16_from_buf(buf: &mut BytesMut) -> Result<i16, DbError> {
    if buf.remaining() < 2 {
        return Err(DbError::protocol("buffer underflow reading i16"));
    }
    Ok(buf.get_i16())
}

/// Read a 32-bit signed integer from a `BytesMut`.
pub fn read_i32_from_buf(buf: &mut BytesMut) -> Result<i32, DbError> {
    if buf.remaining() < 4 {
        return Err(DbError::protocol("buffer underflow reading i32"));
    }
    Ok(buf.get_i32())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_writer_begin_finish_sets_length() {
        let mut w = MessageWriter::new();
        let pos = w.begin(b'T');
        w.put_cstring("hello");
        w.finish(pos);
        let buf = w.finish_message();
        // tag(1) + length(4) + "hello\0"(6) = 11 bytes total
        assert_eq!(buf.len(), 11);
        assert_eq!(buf[0], b'T');
        // length field should be 10 (includes itself: 4 + 6)
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        assert_eq!(len, 10);
    }

    #[test]
    fn message_writer_empty_message() {
        let mut w = MessageWriter::new();
        let pos = w.begin(b'Z');
        w.finish(pos);
        let buf = w.finish_message();
        // tag(1) + length(4) = 5 bytes, length = 4
        assert_eq!(buf.len(), 5);
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        assert_eq!(len, 4);
    }

    #[test]
    fn message_writer_try_finish_rejects_oversized_payload() {
        let mut w = MessageWriter::new();
        let pos = w.begin(b'D');
        w.buf_mut().resize(1 + 4 + MAX_MESSAGE_SIZE + 1, b'x');

        let err = w
            .try_finish(pos)
            .expect_err("oversized backend payload must be rejected");
        assert!(
            err.to_string().contains("backend message too large"),
            "{err}"
        );
        let buf = w.finish_message();
        assert_eq!(&buf[1..5], &[0, 0, 0, 0]);
    }

    #[test]
    fn read_cstring_from_buf_basic() {
        let mut buf = BytesMut::from(&b"hello\0world\0"[..]);
        let s1 = read_cstring_from_buf(&mut buf).unwrap();
        assert_eq!(s1, "hello");
        let s2 = read_cstring_from_buf(&mut buf).unwrap();
        assert_eq!(s2, "world");
        assert!(!buf.has_remaining());
    }

    #[test]
    fn read_cstring_from_buf_empty_string() {
        let mut buf = BytesMut::from(&b"\0"[..]);
        let s = read_cstring_from_buf(&mut buf).unwrap();
        assert_eq!(s, "");
    }

    #[test]
    fn read_cstring_from_buf_no_null_errors() {
        let mut buf = BytesMut::from(&b"hello"[..]);
        let result = read_cstring_from_buf(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn read_cstring_from_buf_with_limit_rejects_oversized_value() {
        let mut buf = BytesMut::from(&b"toolong\0"[..]);
        let result = read_cstring_from_buf_with_limit(&mut buf, 3, "test field");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("test field"), "error: {err_msg}");
        assert!(err_msg.contains("maximum length"), "error: {err_msg}");
    }

    #[test]
    fn read_i32_from_buf_basic() {
        let mut buf = BytesMut::from(&42i32.to_be_bytes()[..]);
        let v = read_i32_from_buf(&mut buf).unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn read_i32_from_buf_underflow() {
        let mut buf = BytesMut::from(&[0u8, 1][..]);
        assert!(read_i32_from_buf(&mut buf).is_err());
    }

    #[test]
    fn read_i16_from_buf_basic() {
        let mut buf = BytesMut::from(&7i16.to_be_bytes()[..]);
        let v = read_i16_from_buf(&mut buf).unwrap();
        assert_eq!(v, 7);
    }

    #[test]
    fn read_i16_from_buf_underflow() {
        let mut buf = BytesMut::from(&[0u8][..]);
        assert!(read_i16_from_buf(&mut buf).is_err());
    }

    #[tokio::test]
    async fn read_startup_v3_basic() {
        // Build a startup message: length(u32) + version(u32) + "user\0test\0\0"
        let mut data = Vec::new();
        let payload: &[u8] = b"\x00\x03\x00\x00user\0test\0\0";
        let len = (payload.len() as u32) + 4;
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await.unwrap();
        match result {
            StartupPayload::Startup(params) => {
                assert_eq!(params.get("user"), Some(&"test".to_string()));
            }
            other => panic!("expected Startup, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_startup_ssl_request() {
        let mut data = Vec::new();
        data.extend_from_slice(&8u32.to_be_bytes()); // length = 8
        data.extend_from_slice(&SSL_REQUEST.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await.unwrap();
        assert!(matches!(result, StartupPayload::SslRequest));
    }

    #[tokio::test]
    async fn read_startup_ssl_request_rejects_trailing_bytes() {
        let mut data = Vec::new();
        data.extend_from_slice(&12u32.to_be_bytes());
        data.extend_from_slice(&SSL_REQUEST.to_be_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("unexpected trailing bytes"),
            "error: {err_msg}"
        );
    }

    #[tokio::test]
    async fn read_frontend_message_query() {
        // Build: 'Q' + length(4+7) + "SELECT\0"
        let mut data = Vec::new();
        data.push(b'Q');
        let payload = b"SELECT\0";
        let len = (payload.len() as u32) + 4;
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(payload);

        let mut cursor = std::io::Cursor::new(data);
        let msg = read_frontend_message(&mut cursor).await.unwrap();
        assert_eq!(msg.tag, b'Q');
        assert_eq!(&msg.payload[..], b"SELECT\0");
    }

    #[tokio::test]
    async fn read_frontend_message_terminate() {
        // Terminate: 'X' + length(4)
        let mut data = Vec::new();
        data.push(b'X');
        data.extend_from_slice(&4u32.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let msg = read_frontend_message(&mut cursor).await.unwrap();
        assert_eq!(msg.tag, b'X');
        assert!(msg.payload.is_empty());
    }

    // -----------------------------------------------------------------------
    // Additional codec tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_startup_cancel_request() {
        let mut data = Vec::new();
        let len = 16u32; // 4 (len) + 4 (cancel magic) + 4 (pid) + 4 (secret)
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&CANCEL_REQUEST.to_be_bytes());
        data.extend_from_slice(&42u32.to_be_bytes()); // pid
        data.extend_from_slice(&99u32.to_be_bytes()); // secret

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await.unwrap();
        match result {
            StartupPayload::CancelRequest(pid, key) => {
                assert_eq!(pid, 42);
                assert_eq!(key, 99);
            }
            other => panic!("expected CancelRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_startup_cancel_request_rejects_trailing_bytes() {
        let mut data = Vec::new();
        let len = 20u32;
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&CANCEL_REQUEST.to_be_bytes());
        data.extend_from_slice(&42u32.to_be_bytes());
        data.extend_from_slice(&99u32.to_be_bytes());
        data.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("cancel request"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_startup_too_short() {
        // A startup message with length < 8 should fail.
        let mut data = Vec::new();
        data.extend_from_slice(&4u32.to_be_bytes()); // length = 4 (too short)

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_startup_unsupported_version() {
        let mut data = Vec::new();
        let len = 8u32;
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // protocol v2

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_startup_cancel_request_too_short() {
        // Cancel request magic but not enough bytes for pid+key.
        let mut data = Vec::new();
        let len = 12u32; // only 4 bytes of payload after magic (need 8)
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&CANCEL_REQUEST.to_be_bytes());
        data.extend_from_slice(&42u32.to_be_bytes()); // only pid, no key

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_frontend_message_length_too_short() {
        // Message with length < 4 should fail.
        let mut data = Vec::new();
        data.push(b'Q');
        data.extend_from_slice(&2u32.to_be_bytes()); // length = 2, invalid

        let mut cursor = std::io::Cursor::new(data);
        let result = read_frontend_message(&mut cursor).await;
        assert!(result.is_err());
    }

    #[test]
    fn message_writer_put_methods() {
        let mut w = MessageWriter::new();
        assert!(w.is_empty());

        w.put_u8(0xFF);
        assert!(!w.is_empty());
        assert_eq!(w.len(), 1);

        w.put_i16(1234);
        assert_eq!(w.len(), 3);

        w.put_u16(5678);
        assert_eq!(w.len(), 5);

        w.put_i32(-1);
        assert_eq!(w.len(), 9);

        w.put_u32(42);
        assert_eq!(w.len(), 13);

        w.put_bytes(b"abc");
        assert_eq!(w.len(), 16);
    }

    #[test]
    fn message_writer_put_cstring_includes_null() {
        let mut w = MessageWriter::new();
        w.put_cstring("hi");
        let buf = w.finish_message();
        assert_eq!(&buf, &[b'h', b'i', 0]);
    }

    #[test]
    fn message_writer_put_cstring_sanitizes_embedded_nulls() {
        let mut w = MessageWriter::new();
        w.put_cstring("hi\0there");
        let buf = w.finish_message();
        assert_eq!(&buf, b"hi there\0");
    }

    #[tokio::test]
    async fn message_writer_flush_clears_buffer() {
        let mut w = MessageWriter::new();
        let pos = w.begin(b'Z');
        w.put_u8(b'I');
        w.finish(pos);
        assert!(!w.is_empty());

        let mut output = Vec::new();
        w.flush(&mut output).await.unwrap();
        assert!(w.is_empty());
        assert!(!output.is_empty());
    }

    #[tokio::test]
    async fn read_startup_v3_multiple_params() {
        // Startup with user + database + trailing null.
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0alice\0database\0mydb\0\0");
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await.unwrap();
        match result {
            StartupPayload::Startup(params) => {
                assert_eq!(params.get("user"), Some(&"alice".to_string()));
                assert_eq!(params.get("database"), Some(&"mydb".to_string()));
            }
            other => panic!("expected Startup, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_startup_v3_rejects_trailing_bytes_after_terminator() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0alice\0\0junk");
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("trailing bytes"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_startup_v3_rejects_missing_final_terminator() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0alice\0");
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("null terminator"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_startup_v3_rejects_duplicate_parameter_keys() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0alice\0user\0bob\0\0");
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("duplicated"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_startup_v3_rejects_too_many_parameters_early() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        for i in 0..=MAX_STARTUP_PARAMS {
            payload.extend_from_slice(format!("k{i}\0v\0").as_bytes());
        }
        payload.push(0);
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("too many startup parameters"),
            "error: {err_msg}"
        );
    }

    #[tokio::test]
    async fn read_startup_v3_rejects_oversized_parameter_name_early() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(
            format!("{}\0v\0\0", "k".repeat(MAX_STARTUP_PARAM_NAME_LEN + 1)).as_bytes(),
        );
        let len = (payload.len() as u32) + 4;

        let mut data = Vec::new();
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("startup parameter name"),
            "error: {err_msg}"
        );
        assert!(err_msg.contains("maximum length"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_startup_rejects_oversized_message() {
        let mut data = Vec::new();
        // Declare a payload of MAX_MESSAGE_SIZE + 1 (won't actually send that much data)
        let payload_len = (MAX_MESSAGE_SIZE + 1) as u32;
        let len = payload_len + 4;
        data.extend_from_slice(&len.to_be_bytes());
        // Only write the version bytes; the size check fires before reading
        data.extend_from_slice(&PROTOCOL_V3.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let result = read_startup(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("too large"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_frontend_message_rejects_oversized_message() {
        let mut data = Vec::new();
        data.push(b'Q');
        let payload_len = (MAX_MESSAGE_SIZE + 1) as u32;
        let len = payload_len + 4;
        data.extend_from_slice(&len.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let result = read_frontend_message(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("too large"), "error: {err_msg}");
    }

    #[tokio::test]
    async fn read_frontend_message_rejects_length_smaller_than_header() {
        let mut data = Vec::new();
        data.push(b'Q');
        data.extend_from_slice(&3u32.to_be_bytes());

        let mut cursor = std::io::Cursor::new(data);
        let result = read_frontend_message(&mut cursor).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("too short"), "error: {err_msg}");
    }

    #[test]
    fn validate_payload_len_rejects_negative_payload_without_wraparound() {
        let result = validate_payload_len(3, 0, "message length too short", "frontend message");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("invalid negative payload length"),
            "error: {err_msg}"
        );
    }

    #[test]
    fn read_cstring_from_buf_invalid_utf8() {
        let mut buf = BytesMut::from(&[0xFF, 0xFE, 0x00][..]);
        let result = read_cstring_from_buf(&mut buf);
        assert!(result.is_err());
    }

    /// Fuzz the startup message parser with pseudo-random byte sequences.
    /// No input must ever cause a panic.
    #[tokio::test]
    async fn read_startup_never_panics_on_random_bytes() {
        let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_F00D;
        for _ in 0..500 {
            // Simple xorshift for deterministic pseudo-random bytes
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let len = (rng_state % 64) as usize;
            let data: Vec<u8> = (0..len)
                .map(|i| {
                    rng_state = rng_state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(i as u64);
                    (rng_state >> 33) as u8
                })
                .collect();
            let mut cursor = std::io::Cursor::new(data);
            let _ = read_startup(&mut cursor).await; // must not panic
        }
    }

    /// Fuzz the frontend message parser with pseudo-random byte sequences.
    #[tokio::test]
    async fn read_frontend_message_never_panics_on_random_bytes() {
        let mut rng_state: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..500 {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let len = (rng_state % 128) as usize;
            let data: Vec<u8> = (0..len)
                .map(|i| {
                    rng_state = rng_state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(i as u64);
                    (rng_state >> 33) as u8
                })
                .collect();
            let mut cursor = std::io::Cursor::new(data);
            let _ = read_frontend_message(&mut cursor).await; // must not panic
        }
    }

    /// Fuzz `read_cstring_from_buf` with varied byte sequences.
    #[test]
    fn read_cstring_from_buf_never_panics_on_random_bytes() {
        let mut rng_state: u64 = 0xAAAA_BBBB_CCCC_DDDD;
        for _ in 0..500 {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let len = (rng_state % 32) as usize;
            let data: Vec<u8> = (0..len)
                .map(|i| {
                    rng_state = rng_state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(i as u64);
                    (rng_state >> 33) as u8
                })
                .collect();
            let mut buf = BytesMut::from(data.as_slice());
            let _ = read_cstring_from_buf(&mut buf); // must not panic
        }
    }
}
