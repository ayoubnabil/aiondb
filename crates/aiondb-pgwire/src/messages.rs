//! Typed frontend (client -> server) and backend (server -> client) messages.
//!
//! Frontend messages are parsed from raw bytes. Backend messages are
//! serialized into the [`MessageWriter`].

use aiondb_core::{DbError, ErrorReport};
use aiondb_engine::ResultColumn;
use bytes::{Buf, Bytes, BytesMut};

use crate::codec::{
    read_cstring_from_buf, read_cstring_from_buf_with_limit, read_i16_from_buf, read_i32_from_buf,
    MessageWriter, MAX_MESSAGE_SIZE,
};

/// Maximum number of parameters allowed in a single Parse or Bind message.
const MAX_STATEMENT_PARAMS: usize = 10_000;
/// Maximum value representable by pgwire `i16` counters.
const MAX_I16_COUNT: usize = 32_767;
/// Maximum length for a single `i32`-sized field in protocol payloads.
const MAX_I32_LENGTH: usize = 2_147_483_647;
/// Maximum payload size for one backend message.
const MAX_BACKEND_MESSAGE_PAYLOAD: usize = MAX_MESSAGE_SIZE;
/// Maximum prepared statement / portal name length accepted from clients.
const MAX_FRONTEND_NAME_BYTES: usize = 1024;
/// Maximum cleartext password response parsed by the generic frontend parser.
const MAX_FRONTEND_PASSWORD_BYTES: usize = 64 * 1024;
/// Maximum client-supplied COPY failure message retained as a string.
const MAX_COPY_FAIL_MESSAGE_BYTES: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Transaction status
// ---------------------------------------------------------------------------

/// Transaction status indicator sent in `ReadyForQuery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle,
    InTransaction,
    Failed,
}

impl TransactionStatus {
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Idle => b'I',
            Self::InTransaction => b'T',
            Self::Failed => b'E',
        }
    }
}

// ---------------------------------------------------------------------------
// Frontend messages (parsed from raw bytes)
// ---------------------------------------------------------------------------

/// Describe target: statement or portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescribeTarget {
    Statement,
    Portal,
}

/// Close target: statement or portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTarget {
    Statement,
    Portal,
}

/// A parsed frontend message.
#[derive(Debug)]
pub enum FrontendMessage {
    /// Simple query: contains the SQL string.
    Query(String),
    /// Parse: (`statement_name`, `query`, `param_oid_list`).
    Parse {
        name: String,
        query: String,
        param_types: Vec<u32>,
    },
    /// Bind: `portal_name`, `statement_name`, `param_formats`, `param_values`, `result_formats`.
    Bind {
        portal: String,
        statement: String,
        param_formats: Vec<i16>,
        param_values: Vec<Option<Bytes>>,
        result_formats: Vec<i16>,
    },
    /// Describe a prepared statement or portal.
    Describe {
        target: DescribeTarget,
        name: String,
    },
    /// Execute a portal with an optional row limit.
    Execute { portal: String, max_rows: i32 },
    /// Sync marks the end of an extended query batch.
    Sync,
    /// Flush requests the server to send pending output.
    Flush,
    /// Close a named statement or portal.
    Close { target: CloseTarget, name: String },
    /// Client is disconnecting.
    Terminate,
    /// Password response from client (tag 'p').
    Password(String),
    /// COPY data from client (tag 'd').
    CopyData(Bytes),
    /// COPY done from client (tag 'c').
    CopyDone,
    /// COPY failed from client (tag 'f').
    CopyFail(String),
}

impl FrontendMessage {
    /// Parse a frontend message from its tag byte and payload.
    pub fn parse(tag: u8, mut payload: BytesMut) -> Result<Self, DbError> {
        match tag {
            b'Q' => {
                let sql = read_cstring_from_buf(&mut payload)?;
                reject_trailing_payload(&payload, "Query")?;
                Ok(Self::Query(sql))
            }
            b'P' => {
                let name = read_frontend_name(&mut payload, "Parse statement name")?;
                let query = read_cstring_from_buf(&mut payload)?;
                let num_params = read_i16_from_buf(&mut payload)?;
                if num_params < 0 {
                    return Err(DbError::protocol(format!(
                        "invalid Parse parameter type count: {num_params}"
                    )));
                }
                let num_params = usize::try_from(num_params).map_err(|_| {
                    DbError::protocol(format!("invalid Parse parameter type count: {num_params}"))
                })?;
                if num_params > MAX_STATEMENT_PARAMS {
                    return Err(DbError::protocol(format!(
                        "too many parameters in Parse message ({num_params}, maximum is {MAX_STATEMENT_PARAMS})"
                    )));
                }
                let mut param_types = Vec::with_capacity(num_params);
                for _ in 0..num_params {
                    let param_type = read_i32_from_buf(&mut payload)?;
                    param_types.push(u32::try_from(param_type).map_err(|_| {
                        DbError::protocol(format!("invalid Parse parameter type oid: {param_type}"))
                    })?);
                }
                Ok(Self::Parse {
                    name,
                    query,
                    param_types,
                })
                .and_then(|message| {
                    reject_trailing_payload(&payload, "Parse")?;
                    Ok(message)
                })
            }
            b'B' => {
                let portal = read_frontend_name(&mut payload, "Bind portal name")?;
                let statement = read_frontend_name(&mut payload, "Bind statement name")?;

                // Parameter format codes.
                let num_formats = read_i16_from_buf(&mut payload)?;
                if num_formats < 0 {
                    return Err(DbError::protocol(format!(
                        "invalid Bind parameter format count: {num_formats}"
                    )));
                }
                let num_formats = usize::try_from(num_formats).map_err(|_| {
                    DbError::protocol(format!(
                        "invalid Bind parameter format count: {num_formats}"
                    ))
                })?;
                if num_formats > MAX_STATEMENT_PARAMS {
                    return Err(DbError::protocol(format!(
                        "too many format codes in Bind message ({num_formats}, maximum is {MAX_STATEMENT_PARAMS})"
                    )));
                }
                let mut param_formats = Vec::with_capacity(num_formats);
                for _ in 0..num_formats {
                    param_formats.push(read_i16_from_buf(&mut payload)?);
                }

                // Parameter values.
                let num_params = read_i16_from_buf(&mut payload)?;
                if num_params < 0 {
                    return Err(DbError::protocol(format!(
                        "invalid Bind parameter value count: {num_params}"
                    )));
                }
                let num_params = usize::try_from(num_params).map_err(|_| {
                    DbError::protocol(format!("invalid Bind parameter value count: {num_params}"))
                })?;
                if num_params > MAX_STATEMENT_PARAMS {
                    return Err(DbError::protocol(format!(
                        "too many parameter values in Bind message ({num_params}, maximum is {MAX_STATEMENT_PARAMS})"
                    )));
                }
                let mut param_values = Vec::with_capacity(num_params);
                for _ in 0..num_params {
                    let len = read_i32_from_buf(&mut payload)?;
                    if len == -1 {
                        param_values.push(None);
                    } else if len < 0 {
                        return Err(DbError::protocol(format!(
                            "invalid Bind parameter value length: {len}"
                        )));
                    } else {
                        let len = usize::try_from(len).map_err(|_| {
                            DbError::protocol(format!("invalid Bind parameter value length: {len}"))
                        })?;
                        if payload.remaining() < len {
                            return Err(DbError::protocol("bind param value truncated"));
                        }
                        let data = payload.split_to(len).freeze();
                        param_values.push(Some(data));
                    }
                }

                // Result format codes.
                let num_result_formats = read_i16_from_buf(&mut payload)?;
                if num_result_formats < 0 {
                    return Err(DbError::protocol(format!(
                        "invalid Bind result format count: {num_result_formats}"
                    )));
                }
                let num_result_formats = usize::try_from(num_result_formats).map_err(|_| {
                    DbError::protocol(format!(
                        "invalid Bind result format count: {num_result_formats}"
                    ))
                })?;
                if num_result_formats > MAX_STATEMENT_PARAMS {
                    return Err(DbError::protocol(format!(
                        "too many result format codes in Bind message ({num_result_formats}, maximum is {MAX_STATEMENT_PARAMS})"
                    )));
                }
                let mut result_formats = Vec::with_capacity(num_result_formats);
                for _ in 0..num_result_formats {
                    result_formats.push(read_i16_from_buf(&mut payload)?);
                }

                Ok(Self::Bind {
                    portal,
                    statement,
                    param_formats,
                    param_values,
                    result_formats,
                })
                .and_then(|message| {
                    reject_trailing_payload(&payload, "Bind")?;
                    Ok(message)
                })
            }
            b'D' => {
                if payload.remaining() < 1 {
                    return Err(DbError::protocol("describe message too short"));
                }
                let kind = payload.get_u8();
                let name = read_frontend_name(&mut payload, "Describe name")?;
                let target = match kind {
                    b'S' => DescribeTarget::Statement,
                    b'P' => DescribeTarget::Portal,
                    _ => {
                        return Err(DbError::protocol(format!(
                            "unknown describe target: {kind}"
                        )));
                    }
                };
                reject_trailing_payload(&payload, "Describe")?;
                Ok(Self::Describe { target, name })
            }
            b'E' => {
                let portal = read_frontend_name(&mut payload, "Execute portal name")?;
                let max_rows = read_i32_from_buf(&mut payload)?;
                if max_rows < 0 {
                    return Err(DbError::protocol(format!(
                        "invalid Execute max_rows: {max_rows}"
                    )));
                }
                reject_trailing_payload(&payload, "Execute")?;
                Ok(Self::Execute { portal, max_rows })
            }
            b'S' => {
                reject_trailing_payload(&payload, "Sync")?;
                Ok(Self::Sync)
            }
            b'H' => {
                reject_trailing_payload(&payload, "Flush")?;
                Ok(Self::Flush)
            }
            b'C' => {
                if payload.remaining() < 1 {
                    return Err(DbError::protocol("close message too short"));
                }
                let kind = payload.get_u8();
                let name = read_frontend_name(&mut payload, "Close name")?;
                let target = match kind {
                    b'S' => CloseTarget::Statement,
                    b'P' => CloseTarget::Portal,
                    _ => return Err(DbError::protocol(format!("unknown close target: {kind}"))),
                };
                reject_trailing_payload(&payload, "Close")?;
                Ok(Self::Close { target, name })
            }
            b'X' => {
                reject_trailing_payload(&payload, "Terminate")?;
                Ok(Self::Terminate)
            }
            b'p' => {
                let password = read_cstring_from_buf_with_limit(
                    &mut payload,
                    MAX_FRONTEND_PASSWORD_BYTES,
                    "Password response",
                )?;
                reject_trailing_payload(&payload, "Password")?;
                Ok(Self::Password(password))
            }
            b'd' => {
                // CopyData: remaining payload is the raw data bytes.
                Ok(Self::CopyData(payload.freeze()))
            }
            b'c' => {
                reject_trailing_payload(&payload, "CopyDone")?;
                Ok(Self::CopyDone)
            }
            b'f' => {
                let error_msg = read_cstring_from_buf_with_limit(
                    &mut payload,
                    MAX_COPY_FAIL_MESSAGE_BYTES,
                    "CopyFail message",
                )?;
                reject_trailing_payload(&payload, "CopyFail")?;
                Ok(Self::CopyFail(error_msg))
            }
            _ => Err(DbError::protocol(format!(
                "unknown frontend message tag: {tag} ('{}')",
                tag as char
            ))),
        }
    }
}

fn read_frontend_name(payload: &mut BytesMut, context: &str) -> Result<String, DbError> {
    read_cstring_from_buf_with_limit(payload, MAX_FRONTEND_NAME_BYTES, context)
}

fn reject_trailing_payload(payload: &BytesMut, message_kind: &str) -> Result<(), DbError> {
    if payload.has_remaining() {
        return Err(DbError::protocol(format!(
            "trailing bytes in {message_kind} message"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Backend message serialization
// ---------------------------------------------------------------------------

/// Column descriptor for `RowDescription`.
#[derive(Debug, Clone)]
pub struct FieldDescription {
    pub name: String,
    pub table_oid: u32,
    pub column_attr: i16,
    pub type_oid: u32,
    pub type_size: i16,
    pub type_modifier: i32,
    pub format_code: i16,
}

/// Write `AuthenticationOk` (tag 'R', auth type 0).
pub fn write_auth_ok(w: &mut MessageWriter) {
    let pos = w.begin(b'R');
    w.put_i32(0); // AuthenticationOk
    w.finish(pos);
}

/// Write `AuthenticationCleartextPassword` (tag 'R', auth type 3).
pub fn write_auth_cleartext_password(w: &mut MessageWriter) {
    let pos = w.begin(b'R');
    w.put_i32(3); // AuthenticationCleartextPassword
    w.finish(pos);
}

/// Write `AuthenticationSASL` (tag 'R', auth type 10).
/// Lists available SASL mechanisms.
pub fn write_auth_sasl(w: &mut MessageWriter, mechanisms: &[&str]) {
    let pos = w.begin(b'R');
    w.put_i32(10); // AuthenticationSASL
    for mechanism in mechanisms {
        w.put_cstring(mechanism);
    }
    w.put_u8(0); // terminator
    w.finish(pos);
}

/// Write `AuthenticationSASLContinue` (tag 'R', auth type 11).
pub fn write_auth_sasl_continue(w: &mut MessageWriter, data: &[u8]) {
    let pos = w.begin(b'R');
    w.put_i32(11); // AuthenticationSASLContinue
    w.put_bytes(data);
    w.finish(pos);
}

/// Write `AuthenticationSASLFinal` (tag 'R', auth type 12).
pub fn write_auth_sasl_final(w: &mut MessageWriter, data: &[u8]) {
    let pos = w.begin(b'R');
    w.put_i32(12); // AuthenticationSASLFinal
    w.put_bytes(data);
    w.finish(pos);
}

/// Write `ParameterStatus` (tag 'S').
pub fn write_parameter_status(w: &mut MessageWriter, key: &str, value: &str) {
    let pos = w.begin(b'S');
    w.put_cstring(key);
    w.put_cstring(value);
    w.finish(pos);
}

/// Write `BackendKeyData` (tag 'K').
pub fn write_backend_key_data(w: &mut MessageWriter, pid: u32, secret: u32) {
    let pos = w.begin(b'K');
    w.put_u32(pid);
    w.put_u32(secret);
    w.finish(pos);
}

/// Write `ReadyForQuery` (tag 'Z').
pub fn write_ready_for_query(w: &mut MessageWriter, status: TransactionStatus) {
    let pos = w.begin(b'Z');
    w.put_u8(status.as_byte());
    w.finish(pos);
}

fn validate_column_count(count: usize, context: &str) -> Result<(), DbError> {
    if count > MAX_I16_COUNT {
        return Err(DbError::internal(format!(
            "too many columns in {context} ({count}, maximum is {})",
            i16::MAX
        )));
    }
    Ok(())
}

/// Write `RowDescription` (tag 'T').
pub fn write_row_description(
    w: &mut MessageWriter,
    fields: &[FieldDescription],
) -> Result<(), DbError> {
    validate_column_count(fields.len(), "result set")?;
    let field_count = i16::try_from(fields.len()).map_err(|_| {
        DbError::internal(format!(
            "too many columns in result set ({}, maximum is {})",
            fields.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b'T');
    w.put_i16(field_count);
    for f in fields {
        w.put_cstring(&f.name);
        w.put_u32(f.table_oid);
        w.put_i16(f.column_attr);
        w.put_u32(f.type_oid);
        w.put_i16(f.type_size);
        w.put_i32(f.type_modifier);
        w.put_i16(f.format_code);
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Write `DataRow` (tag 'D').
///
/// Each column is `Option<&[u8]>`: `None` for NULL, `Some(bytes)` for a value.
pub fn write_data_row(w: &mut MessageWriter, columns: &[Option<&[u8]>]) -> Result<(), DbError> {
    validate_column_count(columns.len(), "data row")?;
    let column_count = i16::try_from(columns.len()).map_err(|_| {
        DbError::internal(format!(
            "too many columns in data row ({}, maximum is {})",
            columns.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b'D');
    w.put_i16(column_count);
    for col in columns {
        match col {
            None => w.put_i32(-1),
            Some(data) => {
                if data.len() > MAX_I32_LENGTH {
                    return Err(DbError::internal(format!(
                        "data row column exceeds pgwire length limit ({}, maximum is {})",
                        data.len(),
                        i32::MAX
                    )));
                }
                let data_len = i32::try_from(data.len()).map_err(|_| {
                    DbError::internal(format!(
                        "data row column length conversion failed ({}, maximum is {})",
                        data.len(),
                        i32::MAX
                    ))
                })?;
                w.put_i32(data_len);
                w.put_bytes(data);
            }
        }
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Write a complete `DataRow` message, encoding each column directly into the
/// `MessageWriter` buffer.
///
/// Uses a length-placeholder/backpatch pattern: for each non-NULL column we
/// reserve 4 bytes for the length, encode the value, then patch the length
/// in-place. This eliminates the intermediate `Vec<Option<Vec<u8>>>` that the
/// simple query path previously required.
///
/// `values` are the row column values.
/// `data_types` are the per-column data types.
/// `text_type_modifiers` are optional per-column text modifiers.
/// `result_formats` is the per-column format-code slice.
pub fn write_data_row_direct(
    w: &mut MessageWriter,
    values: &[aiondb_core::Value],
    data_types: &[aiondb_core::DataType],
    text_type_modifiers: &[Option<aiondb_core::TextTypeModifier>],
    result_formats: &[i16],
) -> Result<(), DbError> {
    validate_column_count(values.len(), "data row")?;
    let column_count = i16::try_from(values.len()).map_err(|_| {
        DbError::internal(format!(
            "too many columns in data row ({}, maximum is {})",
            values.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b'D');
    w.put_i16(column_count);

    for (col_index, value) in values.iter().enumerate() {
        if matches!(value, aiondb_core::Value::Null) {
            w.put_i32(-1);
            continue;
        }
        // Reserve 4 bytes for the length placeholder
        let buf = w.buf_mut();
        let len_offset = buf.len();
        buf.extend_from_slice(&[0u8; 4]);
        let data_start = buf.len();

        // `DataType` carries an owned `Box<DataType>` for arrays and a
        // small struct for vectors, so cloning it per cell costs an
        // allocation for every Array/Vector column on the row. Borrow
        // it directly instead and fall back to a static Text reference
        // when the slice index is out of range (matches `unwrap_or(Text)`).
        const DEFAULT_TEXT: aiondb_core::DataType = aiondb_core::DataType::Text;
        let data_type = data_types.get(col_index).unwrap_or(&DEFAULT_TEXT);
        let text_mod = text_type_modifiers.get(col_index).copied().flatten();
        let wrote = crate::binary_format::encode_column_value_into(
            buf,
            value,
            data_type,
            text_mod,
            result_formats,
            col_index,
        );

        if wrote {
            let data_len = buf.len() - data_start;
            if data_len > MAX_I32_LENGTH {
                return Err(DbError::internal(format!(
                    "data row column exceeds pgwire length limit ({data_len}, maximum is {})",
                    i32::MAX
                )));
            }
            let len_bytes = i32::try_from(data_len)
                .map_err(|_| {
                    DbError::internal(format!(
                        "data row column length conversion failed ({data_len}, maximum is {})",
                        i32::MAX
                    ))
                })?
                .to_be_bytes();
            buf[len_offset..len_offset + 4].copy_from_slice(&len_bytes);
        } else {
            // Value was NULL after all -- rewind and write -1
            buf.truncate(len_offset);
            buf.extend_from_slice(&(-1i32).to_be_bytes());
        }
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Write a complete `DataRow` message, encoding each column directly while
/// reading type/modifier metadata from `columns` to avoid per-row metadata
/// allocation on hot paths.
pub fn write_data_row_direct_from_columns(
    w: &mut MessageWriter,
    values: &[aiondb_core::Value],
    columns: &[ResultColumn],
    result_formats: &[i16],
) -> Result<(), DbError> {
    let binary_flags = resolve_per_column_binary_flags(columns, result_formats);
    write_data_row_direct_from_columns_resolved(w, values, columns, &binary_flags)
}

/// Pre-compute, for each column in `columns`, whether the wire output uses
/// PG binary format (`true`) or text (`false`). Doing this once per batch
/// (instead of per cell) removes the per-cell branch in
/// `resolve_result_format_code` from the inner row encoder loop. Falls back
/// to text when `result_formats` is empty or the per-column slot is missing,
/// matching the broadcast/default behavior of the per-cell resolver.
#[must_use]
pub fn resolve_per_column_binary_flags(
    columns: &[ResultColumn],
    result_formats: &[i16],
) -> Vec<bool> {
    let default_data_type = aiondb_core::DataType::Text;
    (0..columns.len())
        .map(|index| {
            let data_type = columns
                .get(index)
                .map_or(&default_data_type, |column| &column.data_type);
            crate::binary_format::resolve_result_format_code(data_type, result_formats, index) == 1
        })
        .collect()
}

/// Write a `DataRow` using pre-resolved per-column binary/text flags. The
/// caller must produce `binary_flags` via [`resolve_per_column_binary_flags`]
/// (or an equivalent computation) so this hot path performs no per-cell
/// format-code resolution.
pub fn write_data_row_direct_from_columns_resolved(
    w: &mut MessageWriter,
    values: &[aiondb_core::Value],
    columns: &[ResultColumn],
    binary_flags: &[bool],
) -> Result<(), DbError> {
    validate_column_count(values.len(), "data row")?;
    let column_count = i16::try_from(values.len()).map_err(|_| {
        DbError::internal(format!(
            "too many columns in data row ({}, maximum is {})",
            values.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b'D');
    w.put_i16(column_count);

    let default_data_type = aiondb_core::DataType::Text;
    for (col_index, value) in values.iter().enumerate() {
        if matches!(value, aiondb_core::Value::Null) {
            w.put_i32(-1);
            continue;
        }
        let buf = w.buf_mut();
        let len_offset = buf.len();
        buf.extend_from_slice(&[0u8; 4]);
        let data_start = buf.len();

        let (data_type, text_mod) = columns
            .get(col_index)
            .map(|column| (&column.data_type, column.text_type_modifier))
            .unwrap_or((&default_data_type, None));
        let is_binary = binary_flags.get(col_index).copied().unwrap_or(false);
        let wrote = crate::binary_format::encode_column_value_into_resolved(
            buf, value, data_type, text_mod, is_binary,
        );

        if wrote {
            let data_len = buf.len() - data_start;
            if data_len > MAX_I32_LENGTH {
                return Err(DbError::internal(format!(
                    "data row column exceeds pgwire length limit ({data_len}, maximum is {})",
                    i32::MAX
                )));
            }
            let len_bytes = i32::try_from(data_len)
                .map_err(|_| {
                    DbError::internal(format!(
                        "data row column length conversion failed ({data_len}, maximum is {})",
                        i32::MAX
                    ))
                })?
                .to_be_bytes();
            buf[len_offset..len_offset + 4].copy_from_slice(&len_bytes);
        } else {
            buf.truncate(len_offset);
            buf.extend_from_slice(&(-1i32).to_be_bytes());
        }
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Write `CommandComplete` (tag 'C').
pub fn write_command_complete(w: &mut MessageWriter, tag: &str) {
    let pos = w.begin(b'C');
    w.put_cstring(tag);
    w.finish(pos);
}

/// Write a `CommandComplete` whose tag has the form `<prefix> <count>`
/// (or `INSERT 0 <count>` when `insert_zero_prefix` is set), assembling
/// the body directly in the wire buffer so callers don't have to do
/// `format!("SELECT {count}")` on every successful query.
pub fn write_command_complete_with_count(
    w: &mut MessageWriter,
    prefix: &str,
    insert_zero_prefix: bool,
    count: u64,
) {
    let pos = w.begin(b'C');
    let buf = w.buf_mut();
    buf.extend_from_slice(prefix.as_bytes());
    buf.push(b' ');
    if insert_zero_prefix {
        buf.extend_from_slice(b"0 ");
    }
    // Render `count` as ASCII decimal digits into a stack buffer
    // (u64::MAX is 20 digits) and append. No String allocation.
    let mut tmp = [0u8; 20];
    let start = render_u64_decimal(count, &mut tmp);
    buf.extend_from_slice(&tmp[start..]);
    buf.push(0);
    w.finish(pos);
}

/// Render `n` as ASCII decimal digits into the *tail* of `out`,
/// returning the byte index where the rendered digits start.
/// `out` must be at least 20 bytes (u64::MAX is 20 digits).
#[inline]
fn render_u64_decimal(mut n: u64, out: &mut [u8]) -> usize {
    let mut start = out.len();
    if n == 0 {
        start -= 1;
        out[start] = b'0';
    } else {
        while n > 0 {
            start -= 1;
            out[start] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    start
}

/// Write `EmptyQueryResponse` (tag 'I').
pub fn write_empty_query_response(w: &mut MessageWriter) {
    let pos = w.begin(b'I');
    w.finish(pos);
}

/// Write `ErrorResponse` (tag 'E') from a [`DbError`].
pub fn write_error_response(w: &mut MessageWriter, error: &DbError) {
    let report = error.report();
    write_error_response_from_report(w, report);
}

/// Write `NoticeResponse` (tag 'N') with a plain-text notice message.
pub fn write_notice_response(w: &mut MessageWriter, message: &str) {
    let pos = w.begin(b'N');
    // Severity
    w.put_u8(b'S');
    w.put_cstring("NOTICE");
    // Severity (non-localizable)
    w.put_u8(b'V');
    w.put_cstring("NOTICE");
    // SQLSTATE code (`successful_completion`)
    w.put_u8(b'C');
    w.put_cstring("00000");
    // Message
    w.put_u8(b'M');
    w.put_cstring(message);
    // Terminator
    w.put_u8(0);
    w.finish(pos);
}

/// Write `ErrorResponse` (tag 'E') from an [`ErrorReport`].
pub fn write_error_response_from_report(w: &mut MessageWriter, report: &ErrorReport) {
    let pos = w.begin(b'E');
    // Severity
    w.put_u8(b'S');
    w.put_cstring("ERROR");
    // Severity (non-localizable)
    w.put_u8(b'V');
    w.put_cstring("ERROR");
    // SQLSTATE code
    w.put_u8(b'C');
    w.put_cstring(report.sqlstate.code());
    // Message
    w.put_u8(b'M');
    w.put_cstring(&report.message);
    // Optional fields
    if let Some(ref detail) = report.client_detail {
        w.put_u8(b'D');
        w.put_cstring(detail);
    }
    if let Some(ref hint) = report.client_hint {
        w.put_u8(b'H');
        w.put_cstring(hint);
    }
    if let Some(pos_val) = report.position {
        w.put_u8(b'P');
        // Stack-buffer ASCII digit rendering - no String alloc.
        let mut tmp = [0u8; 20];
        let len = render_u64_decimal(pos_val as u64, &mut tmp);
        w.put_bytes(&tmp[len..]);
        w.put_u8(0);
    }
    // Terminator
    w.put_u8(0);
    w.finish(pos);
}

/// Write `ParseComplete` (tag '1').
pub fn write_parse_complete(w: &mut MessageWriter) {
    let pos = w.begin(b'1');
    w.finish(pos);
}

/// Write `BindComplete` (tag '2').
pub fn write_bind_complete(w: &mut MessageWriter) {
    let pos = w.begin(b'2');
    w.finish(pos);
}

/// Write `CloseComplete` (tag '3').
pub fn write_close_complete(w: &mut MessageWriter) {
    let pos = w.begin(b'3');
    w.finish(pos);
}

/// Write `PortalSuspended` (tag 's').
pub fn write_portal_suspended(w: &mut MessageWriter) {
    let pos = w.begin(b's');
    w.finish(pos);
}

/// Write `NoData` (tag 'n').
pub fn write_no_data(w: &mut MessageWriter) {
    let pos = w.begin(b'n');
    w.finish(pos);
}

/// Write `ParameterDescription` (tag 't').
pub fn write_parameter_description(w: &mut MessageWriter, oids: &[u32]) -> Result<(), DbError> {
    let oid_count = i16::try_from(oids.len()).map_err(|_| {
        DbError::internal(format!(
            "too many parameter type OIDs in ParameterDescription ({}, maximum is {})",
            oids.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b't');
    w.put_i16(oid_count);
    for &oid in oids {
        w.put_u32(oid);
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Write `CopyInResponse` (tag 'G').
///
/// Shared implementation for `CopyInResponse`, `CopyOutResponse`, and
/// `CopyBothResponse`.
fn write_copy_response(w: &mut MessageWriter, tag: u8, num_columns: usize) -> Result<(), DbError> {
    let column_count = i16::try_from(num_columns).map_err(|_| {
        DbError::internal(format!(
            "too many columns in COPY response ({num_columns}, maximum is {})",
            i16::MAX
        ))
    })?;
    let pos = w.begin(tag);
    w.put_u8(0); // overall format: text
    w.put_i16(column_count);
    for _ in 0..num_columns {
        w.put_i16(0); // per-column format: text
    }
    w.try_finish(pos)?;
    Ok(())
}

/// Tells the client to start sending COPY data.
pub fn write_copy_in_response(w: &mut MessageWriter, num_columns: usize) -> Result<(), DbError> {
    write_copy_response(w, b'G', num_columns)
}

/// Write `CopyOutResponse` (tag 'H').
///
/// Tells the client that the server will send COPY data.
pub fn write_copy_out_response(w: &mut MessageWriter, num_columns: usize) -> Result<(), DbError> {
    write_copy_response(w, b'H', num_columns)
}

/// Write `CopyBothResponse` (tag 'W').
///
/// Tells the client that both sides will exchange `CopyData` frames.
pub fn write_copy_both_response(w: &mut MessageWriter, num_columns: usize) -> Result<(), DbError> {
    write_copy_response(w, b'W', num_columns)
}

/// Write `CopyData` (tag 'd').
///
/// Sends a chunk of COPY data (one or more rows).
pub fn write_copy_data(w: &mut MessageWriter, data: &[u8]) -> Result<(), DbError> {
    if data.is_empty() {
        // PG never emits an empty CopyData frame; libpq logs a warning if
        // batches without polluting the wire.
        return Ok(());
    }

    for chunk in data.chunks(MAX_BACKEND_MESSAGE_PAYLOAD) {
        let pos = w.begin(b'd');
        w.put_bytes(chunk);
        w.try_finish(pos)?;
    }
    Ok(())
}

/// Write a single COPY data row whose payload is `line` followed by
/// a `\n` newline terminator. Saves the per-row `Vec<u8>` allocation
/// that `write_copy_out_result` previously paid to splice the newline
/// onto the line bytes.
///
/// For long lines, emit the line in bounded chunks and then a final newline
/// frame. COPY data is a byte stream, so rows may span `CopyData` frames.
pub fn write_copy_data_line(w: &mut MessageWriter, line: &str) -> Result<(), DbError> {
    let bytes = line.as_bytes();
    if bytes.len() < MAX_BACKEND_MESSAGE_PAYLOAD {
        let pos = w.begin(b'd');
        w.put_bytes(bytes);
        w.put_u8(b'\n');
        w.try_finish(pos)?;
    } else {
        write_copy_data(w, bytes)?;
        write_copy_data(w, b"\n")?;
    }
    Ok(())
}

/// Write `CopyDone` (tag 'c').
///
/// Signals the end of COPY data from the server.
pub fn write_copy_done(w: &mut MessageWriter) {
    let pos = w.begin(b'c');
    w.finish(pos);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
