mod binary_io;
mod catalog;

use aiondb_core::{
    convert::usize_to_u64_saturating, ColumnId, DataType, DbError, DbResult, IndexId,
    IntervalValue, NumericValue, PgDate, RelationId, Row, TidValue, TupleId, TxnId, Value,
    VectorValue,
};
use aiondb_storage_api::{
    IndexKeyColumn, IndexStorageDescriptor, StorageColumn, StorageShardConfig,
    TableStorageDescriptor, MAX_STORAGE_HASH_RING_VIRTUAL_NODES, MAX_STORAGE_SHARD_COUNT,
    MAX_STORAGE_VIRTUAL_NODES_PER_SHARD,
};
use aiondb_tx::IsolationLevel;

use crate::lsn::Lsn;
use crate::record::{WalEntry, WalRecord};
use crate::WalCompression;
use binary_io::{BinaryReader, BinaryWriter};

/// Minimum number of bytes needed to read the `payload_len` header.
pub const ENTRY_HEADER_SIZE: usize = 4;

/// Marker byte introducing the v2 WAL entry payload envelope.
///
/// Legacy payloads start directly with a record tag in range `0..=43`.
/// `0xFF` is reserved to unambiguously identify the framed format.
const ENTRY_V2_MARKER: u8 = 0xFF;
/// Legacy framed payload format version (compression envelope only).
const ENTRY_FRAMED_FORMAT_VERSION_V1: u8 = 1;
/// Framed payload format adding `prev_lsn` (`xl_prev`-style backward chaining).
const ENTRY_FRAMED_FORMAT_VERSION_V2: u8 = 2;
/// Current framed payload format version.
///
/// Adds `database_id` (u32 LE) after `prev_lsn`. ADR-0014 phase 4bis.
const ENTRY_FRAMED_FORMAT_VERSION_V3: u8 = 3;

const ENTRY_COMPRESSION_NONE: u8 = 0;
const ENTRY_COMPRESSION_LZ4: u8 = 1;
const ENTRY_COMPRESSION_ZSTD: u8 = 2;

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------

use aiondb_core::checksum::{compute_crc32c, compute_legacy_fnv1a};

// ---------------------------------------------------------------------------
// Value encoding/decoding
// ---------------------------------------------------------------------------

// ── Date/time serialization helpers ──────────────────────────────────

fn write_date(w: &mut BinaryWriter, date: time::Date) {
    w.write_i32(date.year());
    w.write_u8(date.month() as u8);
    w.write_u8(date.day());
}

fn write_time(w: &mut BinaryWriter, time: time::Time) {
    w.write_u8(time.hour());
    w.write_u8(time.minute());
    w.write_u8(time.second());
    w.write_u32(time.nanosecond());
}

fn read_date(r: &mut BinaryReader) -> DbResult<time::Date> {
    let year = r.read_i32()?;
    let month_u8 = r.read_u8()?;
    let day = r.read_u8()?;
    let month =
        time::Month::try_from(month_u8).map_err(|_| DbError::internal("WAL: invalid month"))?;
    time::Date::from_calendar_date(year, month, day)
        .map_err(|_| DbError::internal("WAL: invalid date"))
}

fn read_time(r: &mut BinaryReader) -> DbResult<time::Time> {
    let hour = r.read_u8()?;
    let minute = r.read_u8()?;
    let second = r.read_u8()?;
    let nano = r.read_u32()?;
    time::Time::from_hms_nano(hour, minute, second, nano)
        .map_err(|_| DbError::internal("WAL: invalid time"))
}

// ─────────────────────────────────────────────────────────────────────

fn write_value(w: &mut BinaryWriter, val: &Value) -> DbResult<()> {
    write_value_impl(w, val, 0)
}

fn write_value_impl(w: &mut BinaryWriter, val: &Value, depth: usize) -> DbResult<()> {
    if depth > MAX_NESTING_DEPTH {
        return Err(DbError::internal("WAL: value nesting depth exceeded"));
    }
    match val {
        Value::Null => w.write_u8(0),
        Value::Int(v) => {
            w.write_u8(1);
            w.write_i32(*v);
        }
        Value::BigInt(v) => {
            w.write_u8(2);
            w.write_i64(*v);
        }
        Value::Real(v) => {
            w.write_u8(3);
            w.write_f32(*v);
        }
        Value::Double(v) => {
            w.write_u8(4);
            w.write_f64(*v);
        }
        Value::Numeric(n) => {
            if n.is_big() {
                // Big numeric: tag 25, length-prefixed decimal string + scale
                w.write_u8(25);
                let coeff_str = n.coefficient_to_string();
                w.write_str(&coeff_str)?;
                w.write_u32(n.scale);
            } else {
                // Small numeric: backward-compatible i128 format
                w.write_u8(5);
                w.write_i128(n.coefficient);
                w.write_u32(n.scale);
            }
        }
        Value::Money(v) => {
            w.write_u8(24);
            w.write_i64(*v);
        }
        Value::Text(s) => {
            w.write_u8(6);
            w.write_str(s)?;
        }
        Value::Boolean(v) => {
            w.write_u8(7);
            w.write_bool(*v);
        }
        Value::Blob(b) => {
            w.write_u8(8);
            w.write_bytes(b)?;
        }
        Value::Timestamp(dt) => {
            w.write_u8(9);
            write_date(w, dt.date());
            write_time(w, dt.time());
        }
        Value::Date(d) => {
            w.write_u8(10);
            write_date(w, *d);
        }
        Value::LargeDate(d) => {
            w.write_u8(23);
            w.write_i32(d.year());
            w.write_u8(d.month() as u8);
            w.write_u8(d.day());
        }
        Value::Time(t) => {
            w.write_u8(17);
            write_time(w, *t);
        }
        Value::TimeTz(t, offset) => {
            w.write_u8(18);
            write_time(w, *t);
            w.write_i32(offset.whole_seconds());
        }
        Value::Interval(iv) => {
            w.write_u8(11);
            w.write_i32(iv.months);
            w.write_i32(iv.days);
            w.write_i64(iv.micros);
        }
        Value::Tid(value) => {
            w.write_u8(22);
            w.write_u32(value.block());
            w.write_u16(value.offset());
        }
        Value::Uuid(bytes) => {
            w.write_u8(13);
            w.write_raw(bytes);
        }
        Value::TimestampTz(odt) => {
            w.write_u8(14);
            write_date(w, odt.date());
            write_time(w, odt.time());
            let (oh, om, os) = odt.offset().as_hms();
            w.write_i32(i32::from(oh) * 3600 + i32::from(om) * 60 + i32::from(os));
        }
        Value::PgLsn(value) => {
            w.write_u8(21);
            w.write_u64(value.raw());
        }
        Value::Vector(vv) => {
            let dims_usize = validate_vector_dims_for_wal(vv.dims)?;
            if dims_usize != vv.values.len() {
                return Err(DbError::internal(format!(
                    "WAL: vector dimensions {} do not match {} encoded values",
                    vv.dims,
                    vv.values.len()
                )));
            }
            w.write_u8(12);
            w.write_u32(vv.dims);
            for &f in &vv.values {
                w.write_f32(f);
            }
        }
        Value::Jsonb(v) => {
            w.write_u8(16);
            w.write_str(&v.to_string())?;
        }
        Value::Array(elems) => {
            w.write_u8(15);
            write_bounded_u32_count(w, "array length", elems.len(), MAX_COLLECTION_ITEMS)?;
            for elem in elems {
                write_value_impl(w, elem, depth + 1)?;
            }
        }
        Value::MacAddr(value) => {
            w.write_u8(19);
            w.write_raw(value.as_bytes());
        }
        Value::MacAddr8(value) => {
            w.write_u8(20);
            w.write_raw(value.as_bytes());
        }
    }
    Ok(())
}

/// Maximum nesting depth for recursive Value/DataType decoding to prevent
/// stack overflow from crafted WAL records.
const MAX_NESTING_DEPTH: usize = 128;
/// Upper bound for decoded collection lengths from WAL payloads.
const MAX_COLLECTION_ITEMS: usize = 1_000_000;
/// Upper bound for decoded vector dimensions from WAL payloads.
const MAX_VECTOR_DIMS: usize = 1_000_000;
/// Upper bound for decoded column ordinals in WAL payloads.
const MAX_COLUMN_ORDINAL: usize = 1_000_000;
/// Upper bound for full-page-image payload bytes.
const MAX_FULL_PAGE_IMAGE_BYTES: usize = 64 * 1024;

#[inline]
fn u32_to_usize_checked(value: u32, field_name: &str) -> DbResult<usize> {
    usize::try_from(value)
        .map_err(|_| DbError::internal(format!("WAL: {field_name} exceeds platform usize range")))
}

fn read_bounded_u32_count(
    r: &mut BinaryReader<'_>,
    field_name: &str,
    limit: usize,
) -> DbResult<usize> {
    let count = u32_to_usize_checked(r.read_u32()?, field_name)?;
    if count > limit {
        return Err(DbError::internal(format!(
            "WAL: {field_name} {count} exceeds limit {limit}"
        )));
    }
    Ok(count)
}

fn read_bounded_u64_usize(
    r: &mut BinaryReader<'_>,
    field_name: &str,
    limit: usize,
) -> DbResult<usize> {
    let raw = r.read_u64()?;
    let value = usize::try_from(raw).map_err(|_| {
        DbError::internal(format!(
            "WAL: {field_name} {raw} exceeds platform usize range"
        ))
    })?;
    if value > limit {
        return Err(DbError::internal(format!(
            "WAL: {field_name} {value} exceeds limit {limit}"
        )));
    }
    Ok(value)
}

fn write_bounded_u32_count(
    w: &mut BinaryWriter,
    field_name: &str,
    count: usize,
    limit: usize,
) -> DbResult<()> {
    if count > limit {
        return Err(DbError::internal(format!(
            "WAL: {field_name} {count} exceeds limit {limit}"
        )));
    }
    let raw = u32::try_from(count).map_err(|_| {
        DbError::internal(format!(
            "WAL: {field_name} {count} exceeds u32 encoding range"
        ))
    })?;
    w.write_u32(raw);
    Ok(())
}

fn validate_vector_dims_for_wal(dims: u32) -> DbResult<usize> {
    let dims_usize = u32_to_usize_checked(dims, "vector dimensions")?;
    if dims_usize > MAX_VECTOR_DIMS {
        return Err(DbError::internal(format!(
            "WAL: vector dimensions {dims_usize} exceed limit {MAX_VECTOR_DIMS}"
        )));
    }
    Ok(dims_usize)
}

fn read_value(r: &mut BinaryReader<'_>) -> DbResult<Value> {
    read_value_impl(r, 0)
}

fn read_value_impl(r: &mut BinaryReader<'_>, depth: usize) -> DbResult<Value> {
    if depth > MAX_NESTING_DEPTH {
        return Err(DbError::internal("WAL: value nesting depth exceeded"));
    }
    let tag = r.read_u8()?;
    match tag {
        0 => Ok(Value::Null),
        1 => Ok(Value::Int(r.read_i32()?)),
        2 => Ok(Value::BigInt(r.read_i64()?)),
        3 => Ok(Value::Real(r.read_f32()?)),
        4 => Ok(Value::Double(r.read_f64()?)),
        5 => {
            let coeff = r.read_i128()?;
            let scale = r.read_u32()?;
            Ok(Value::Numeric(NumericValue::new(coeff, scale)))
        }
        24 => Ok(Value::Money(r.read_i64()?)),
        6 => Ok(Value::Text(r.read_str()?)),
        7 => Ok(Value::Boolean(r.read_bool()?)),
        8 => Ok(Value::Blob(r.read_bytes()?)),
        9 => {
            let date = read_date(r)?;
            let time = read_time(r)?;
            Ok(Value::Timestamp(time::PrimitiveDateTime::new(date, time)))
        }
        10 => Ok(Value::Date(read_date(r)?)),
        23 => {
            let year = r.read_i32()?;
            let month_u8 = r.read_u8()?;
            let day = r.read_u8()?;
            let month = time::Month::try_from(month_u8)
                .map_err(|_| DbError::internal("WAL: invalid month"))?;
            let date = PgDate::from_calendar_date(year, month, day)
                .map_err(|()| DbError::internal("WAL: invalid large date"))?;
            Ok(Value::LargeDate(date))
        }
        11 => {
            let months = r.read_i32()?;
            let days = r.read_i32()?;
            let micros = r.read_i64()?;
            Ok(Value::Interval(IntervalValue::new(months, days, micros)))
        }
        12 => {
            let dims = r.read_u32()?;
            let dims_usize = validate_vector_dims_for_wal(dims)?;
            let mut values = Vec::with_capacity(r.capped_capacity(dims_usize));
            for _ in 0..dims_usize {
                values.push(r.read_f32()?);
            }
            Ok(Value::Vector(VectorValue::new(dims, values)))
        }
        13 => {
            let mut bytes = [0u8; 16];
            for b in &mut bytes {
                *b = r.read_u8()?;
            }
            Ok(Value::Uuid(bytes))
        }
        14 => {
            let date = read_date(r)?;
            let time_val = read_time(r)?;
            let offset_secs = r.read_i32()?;
            let offset = time::UtcOffset::from_whole_seconds(offset_secs)
                .map_err(|_| DbError::internal("WAL: invalid offset"))?;
            let pdt = time::PrimitiveDateTime::new(date, time_val);
            Ok(Value::TimestampTz(pdt.assume_offset(offset)))
        }
        15 => {
            let len = read_bounded_u32_count(r, "array length", MAX_COLLECTION_ITEMS)?;
            let mut elems = Vec::with_capacity(r.capped_capacity(len));
            for _ in 0..len {
                elems.push(read_value_impl(r, depth + 1)?);
            }
            Ok(Value::Array(elems))
        }
        16 => {
            let s = r.read_str()?;
            let v: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| DbError::internal(format!("WAL: invalid JSONB: {e}")))?;
            Ok(Value::Jsonb(v))
        }
        17 => Ok(Value::Time(read_time(r)?)),
        18 => {
            let t = read_time(r)?;
            let offset_secs = r.read_i32()?;
            let offset = time::UtcOffset::from_whole_seconds(offset_secs)
                .map_err(|_| DbError::internal("WAL: invalid offset"))?;
            Ok(Value::TimeTz(t, offset))
        }
        19 => {
            let mut bytes = [0u8; 6];
            for byte in &mut bytes {
                *byte = r.read_u8()?;
            }
            Ok(Value::MacAddr(aiondb_core::MacAddr::new(bytes)))
        }
        20 => {
            let mut bytes = [0u8; 8];
            for byte in &mut bytes {
                *byte = r.read_u8()?;
            }
            Ok(Value::MacAddr8(aiondb_core::MacAddr8::new(bytes)))
        }
        21 => Ok(Value::PgLsn(aiondb_core::PgLsnValue::new(r.read_u64()?))),
        22 => Ok(Value::Tid(TidValue::new(r.read_u32()?, r.read_u16()?))),
        25 => {
            // Big numeric: coefficient stored as decimal string + scale
            let coeff_str = r.read_str()?;
            let scale = r.read_u32()?;
            let nv = NumericValue::from_coefficient_string(&coeff_str, scale)
                .map_err(|e| DbError::internal(format!("WAL: invalid big numeric: {e}")))?;
            Ok(Value::Numeric(nv))
        }
        _ => Err(DbError::internal(format!("WAL: unknown value tag {tag}"))),
    }
}

// ---------------------------------------------------------------------------
// DataType encoding/decoding
// ---------------------------------------------------------------------------

fn write_data_type(w: &mut BinaryWriter, dt: &DataType) -> DbResult<()> {
    write_data_type_impl(w, dt, 0)
}

fn write_data_type_impl(w: &mut BinaryWriter, dt: &DataType, depth: usize) -> DbResult<()> {
    if depth > MAX_NESTING_DEPTH {
        return Err(DbError::internal("WAL: DataType nesting depth exceeded"));
    }
    match dt {
        DataType::Int => w.write_u8(0),
        DataType::BigInt => w.write_u8(1),
        DataType::Real => w.write_u8(2),
        DataType::Double => w.write_u8(3),
        DataType::Numeric => w.write_u8(4),
        DataType::Money => w.write_u8(22),
        DataType::Text => w.write_u8(5),
        DataType::Boolean => w.write_u8(6),
        DataType::Blob => w.write_u8(7),
        DataType::Timestamp => w.write_u8(8),
        DataType::Date => w.write_u8(9),
        DataType::Interval => w.write_u8(10),
        DataType::Vector { dims, element_type } => {
            validate_vector_dims_for_wal(*dims)?;
            if *element_type == aiondb_core::VectorElementType::Float32 {
                // Tag 11 keeps the compact Float32 vector format.
                w.write_u8(11);
                w.write_u32(*dims);
            } else {
                // Tag 30: extended format with element type byte.
                w.write_u8(30);
                w.write_u32(*dims);
                w.write_u8(match element_type {
                    aiondb_core::VectorElementType::Float32 => 0,
                    aiondb_core::VectorElementType::Float16 => 1,
                    aiondb_core::VectorElementType::Uint8 => 2,
                });
            }
        }
        DataType::Uuid => w.write_u8(12),
        DataType::TimestampTz => w.write_u8(13),
        DataType::Jsonb => w.write_u8(15),
        DataType::Time => w.write_u8(16),
        DataType::TimeTz => w.write_u8(17),
        DataType::MacAddr => w.write_u8(18),
        DataType::MacAddr8 => w.write_u8(19),
        DataType::PgLsn => w.write_u8(20),
        DataType::Tid => w.write_u8(21),
        DataType::Array(inner) => {
            w.write_u8(14);
            write_data_type_impl(w, inner, depth + 1)?;
        }
    }
    Ok(())
}

fn read_data_type(r: &mut BinaryReader<'_>) -> DbResult<DataType> {
    read_data_type_impl(r, 0)
}

fn read_data_type_impl(r: &mut BinaryReader<'_>, depth: usize) -> DbResult<DataType> {
    if depth > MAX_NESTING_DEPTH {
        return Err(DbError::internal("WAL: DataType nesting depth exceeded"));
    }
    let tag = r.read_u8()?;
    match tag {
        0 => Ok(DataType::Int),
        1 => Ok(DataType::BigInt),
        2 => Ok(DataType::Real),
        3 => Ok(DataType::Double),
        4 => Ok(DataType::Numeric),
        22 => Ok(DataType::Money),
        5 => Ok(DataType::Text),
        6 => Ok(DataType::Boolean),
        7 => Ok(DataType::Blob),
        8 => Ok(DataType::Timestamp),
        9 => Ok(DataType::Date),
        10 => Ok(DataType::Interval),
        11 => {
            // Legacy format: Float32 only.
            let dims = r.read_u32()?;
            validate_vector_dims_for_wal(dims)?;
            Ok(DataType::Vector {
                dims,
                element_type: aiondb_core::VectorElementType::Float32,
            })
        }
        30 => {
            // Extended format with element type byte.
            let dims = r.read_u32()?;
            validate_vector_dims_for_wal(dims)?;
            let element_type = match r.read_u8()? {
                0 => aiondb_core::VectorElementType::Float32,
                1 => aiondb_core::VectorElementType::Float16,
                2 => aiondb_core::VectorElementType::Uint8,
                other => {
                    return Err(DbError::internal(format!(
                        "WAL: unknown vector element type tag {other}"
                    )));
                }
            };
            Ok(DataType::Vector { dims, element_type })
        }
        12 => Ok(DataType::Uuid),
        13 => Ok(DataType::TimestampTz),
        14 => {
            let inner = read_data_type_impl(r, depth + 1)?;
            Ok(DataType::Array(Box::new(inner)))
        }
        15 => Ok(DataType::Jsonb),
        16 => Ok(DataType::Time),
        17 => Ok(DataType::TimeTz),
        18 => Ok(DataType::MacAddr),
        19 => Ok(DataType::MacAddr8),
        20 => Ok(DataType::PgLsn),
        21 => Ok(DataType::Tid),
        _ => Err(DbError::internal(format!(
            "WAL: unknown DataType tag {tag}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Row encoding/decoding
// ---------------------------------------------------------------------------

fn write_row(w: &mut BinaryWriter, row: &Row) -> DbResult<()> {
    write_bounded_u32_count(w, "row value count", row.values.len(), MAX_COLLECTION_ITEMS)?;
    for v in &row.values {
        write_value(w, v)?;
    }
    Ok(())
}

fn read_row(r: &mut BinaryReader<'_>) -> DbResult<Row> {
    let count = read_bounded_u32_count(r, "row value count", MAX_COLLECTION_ITEMS)?;
    let mut values = Vec::with_capacity(r.capped_capacity(count));
    for _ in 0..count {
        values.push(read_value(r)?);
    }
    Ok(Row::new(values))
}

// ---------------------------------------------------------------------------
// StorageColumn encoding/decoding
// ---------------------------------------------------------------------------

fn write_storage_column(w: &mut BinaryWriter, col: &StorageColumn) -> DbResult<()> {
    w.write_u64(col.column_id.get());
    write_data_type(w, &col.data_type)?;
    w.write_bool(col.nullable);
    Ok(())
}

fn read_storage_column(r: &mut BinaryReader<'_>) -> DbResult<StorageColumn> {
    let column_id = ColumnId::new(r.read_u64()?);
    let data_type = read_data_type(r)?;
    let nullable = r.read_bool()?;
    Ok(StorageColumn {
        column_id,
        data_type,
        nullable,
    })
}

// ---------------------------------------------------------------------------
// TableStorageDescriptor encoding/decoding
// ---------------------------------------------------------------------------

fn validate_wal_shard_config(sc: &StorageShardConfig) -> DbResult<()> {
    if sc.shard_key_columns.is_empty() {
        return Err(DbError::internal(
            "WAL: shard key column count must be >= 1",
        ));
    }
    if sc.shard_key_columns.len() > MAX_COLLECTION_ITEMS {
        return Err(DbError::internal(format!(
            "WAL: shard key column count {} exceeds maximum {MAX_COLLECTION_ITEMS}",
            sc.shard_key_columns.len()
        )));
    }
    if sc.shard_count == 0 {
        return Err(DbError::internal("WAL: shard_count must be >= 1"));
    }
    if sc.shard_count > MAX_STORAGE_SHARD_COUNT {
        return Err(DbError::internal(format!(
            "WAL: shard_count must be <= {MAX_STORAGE_SHARD_COUNT}"
        )));
    }
    if sc.virtual_nodes_per_shard == 0 {
        return Err(DbError::internal(
            "WAL: virtual_nodes_per_shard must be >= 1",
        ));
    }
    if sc.virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD {
        return Err(DbError::internal(format!(
            "WAL: virtual_nodes_per_shard must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD}"
        )));
    }
    let total_virtual_nodes = u64::from(sc.shard_count) * u64::from(sc.virtual_nodes_per_shard);
    if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
        return Err(DbError::internal(format!(
            "WAL: shard hash ring would contain {total_virtual_nodes} virtual nodes, exceeding {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
        )));
    }
    Ok(())
}

fn write_table_descriptor(w: &mut BinaryWriter, desc: &TableStorageDescriptor) -> DbResult<()> {
    w.write_u64(desc.table_id.get());
    write_bounded_u32_count(
        w,
        "table column count",
        desc.columns.len(),
        MAX_COLLECTION_ITEMS,
    )?;
    for col in &desc.columns {
        write_storage_column(w, col)?;
    }
    match &desc.primary_key {
        Some(pk) => {
            w.write_bool(true);
            write_bounded_u32_count(
                w,
                "primary key column count",
                pk.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for col_id in pk {
                w.write_u64(col_id.get());
            }
        }
        None => {
            w.write_bool(false);
        }
    }
    // Shard config (WAL v2 extension - backwards compatible via has_shard flag).
    match &desc.shard_config {
        Some(sc) => {
            validate_wal_shard_config(sc)?;
            w.write_bool(true);
            write_bounded_u32_count(
                w,
                "shard key column count",
                sc.shard_key_columns.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for col_id in &sc.shard_key_columns {
                w.write_u64(col_id.get());
            }
            w.write_u32(sc.shard_count);
            // hash_function: 0 = Sha256 (only variant for now).
            w.write_u8(0);
            w.write_u32(sc.virtual_nodes_per_shard);
        }
        None => {
            w.write_bool(false);
        }
    }
    Ok(())
}

fn read_table_descriptor(r: &mut BinaryReader<'_>) -> DbResult<TableStorageDescriptor> {
    let table_id = RelationId::new(r.read_u64()?);
    let col_count = read_bounded_u32_count(r, "table column count", MAX_COLLECTION_ITEMS)?;
    let mut columns = Vec::with_capacity(r.capped_capacity(col_count));
    for _ in 0..col_count {
        columns.push(read_storage_column(r)?);
    }
    let has_pk = r.read_bool()?;
    let primary_key = if has_pk {
        let pk_count = read_bounded_u32_count(r, "primary key column count", MAX_COLLECTION_ITEMS)?;
        let mut pk = Vec::with_capacity(r.capped_capacity(pk_count));
        for _ in 0..pk_count {
            pk.push(ColumnId::new(r.read_u64()?));
        }
        Some(pk)
    } else {
        None
    };
    // Shard config (WAL v2 extension). Old WAL segments without this
    // field will hit EOF here - treat that as "no shard config".
    let shard_config = if r.remaining() > 0 && r.read_bool()? {
        let key_count = read_bounded_u32_count(r, "shard key column count", MAX_COLLECTION_ITEMS)?;
        if key_count == 0 {
            return Err(DbError::internal(
                "WAL: shard key column count must be >= 1",
            ));
        }
        let mut shard_key_columns = Vec::with_capacity(r.capped_capacity(key_count));
        for _ in 0..key_count {
            shard_key_columns.push(ColumnId::new(r.read_u64()?));
        }
        let shard_count = r.read_u32()?;
        if shard_count == 0 {
            return Err(DbError::internal("WAL: shard_count must be >= 1"));
        }
        if shard_count > MAX_STORAGE_SHARD_COUNT {
            return Err(DbError::internal(format!(
                "WAL: shard_count must be <= {MAX_STORAGE_SHARD_COUNT}"
            )));
        }
        let hash_fn_tag = r.read_u8()?; // currently only 0 = Sha256
        if hash_fn_tag != 0 {
            return Err(DbError::internal(format!(
                "WAL: unknown shard hash function tag {hash_fn_tag}"
            )));
        }
        let virtual_nodes_per_shard = r.read_u32()?;
        if virtual_nodes_per_shard == 0 {
            return Err(DbError::internal(
                "WAL: virtual_nodes_per_shard must be >= 1",
            ));
        }
        if virtual_nodes_per_shard > MAX_STORAGE_VIRTUAL_NODES_PER_SHARD {
            return Err(DbError::internal(format!(
                "WAL: virtual_nodes_per_shard must be <= {MAX_STORAGE_VIRTUAL_NODES_PER_SHARD}"
            )));
        }
        let total_virtual_nodes = u64::from(shard_count) * u64::from(virtual_nodes_per_shard);
        if total_virtual_nodes > MAX_STORAGE_HASH_RING_VIRTUAL_NODES {
            return Err(DbError::internal(format!(
                "WAL: shard hash ring would contain {total_virtual_nodes} virtual nodes, exceeding {MAX_STORAGE_HASH_RING_VIRTUAL_NODES}"
            )));
        }
        Some(StorageShardConfig {
            shard_key_columns,
            shard_count,
            hash_function: aiondb_storage_api::ShardHashFunction::Sha256,
            virtual_nodes_per_shard,
        })
    } else {
        None
    };
    Ok(TableStorageDescriptor {
        table_id,
        columns,
        primary_key,
        shard_config,
    })
}

// ---------------------------------------------------------------------------
// IndexKeyColumn encoding/decoding
// ---------------------------------------------------------------------------

fn write_index_key_column(w: &mut BinaryWriter, kc: &IndexKeyColumn) {
    w.write_u64(kc.column_id.get());
    w.write_bool(kc.descending);
    w.write_bool(kc.nulls_first);
}

fn read_index_key_column(r: &mut BinaryReader<'_>) -> DbResult<IndexKeyColumn> {
    let column_id = ColumnId::new(r.read_u64()?);
    let descending = r.read_bool()?;
    let nulls_first = r.read_bool()?;
    Ok(IndexKeyColumn {
        column_id,
        descending,
        nulls_first,
    })
}

// ---------------------------------------------------------------------------
// IndexStorageDescriptor encoding/decoding
// ---------------------------------------------------------------------------

fn write_index_descriptor(w: &mut BinaryWriter, desc: &IndexStorageDescriptor) -> DbResult<()> {
    w.write_u64(desc.index_id.get());
    w.write_u64(desc.table_id.get());
    w.write_bool(desc.unique);
    write_bounded_u32_count(
        w,
        "index key column count",
        desc.key_columns.len(),
        MAX_COLLECTION_ITEMS,
    )?;
    for kc in &desc.key_columns {
        write_index_key_column(w, kc);
    }
    write_bounded_u32_count(
        w,
        "index include column count",
        desc.include_columns.len(),
        MAX_COLLECTION_ITEMS,
    )?;
    for col_id in &desc.include_columns {
        w.write_u64(col_id.get());
    }
    match &desc.hnsw_options {
        None => w.write_u8(0),
        Some(options) => {
            // Tag 2: same as tag 1 plus a trailing `prenormalised` bool.
            // Older entries written with tag 1 are still accepted on read.
            w.write_u8(2);
            w.write_u32(options.m);
            w.write_u32(options.ef_construction);
            w.write_u8(stored_metric_tag(options.distance_metric));
            w.write_u8(stored_quantization_tag(options.quantization));
            w.write_bool(options.prenormalised);
        }
    }
    w.write_bool(desc.gin);
    w.write_bool(desc.nulls_not_distinct);
    Ok(())
}

fn read_index_descriptor(r: &mut BinaryReader<'_>) -> DbResult<IndexStorageDescriptor> {
    let index_id = IndexId::new(r.read_u64()?);
    let table_id = RelationId::new(r.read_u64()?);
    let unique = r.read_bool()?;
    let key_count = read_bounded_u32_count(r, "index key column count", MAX_COLLECTION_ITEMS)?;
    let mut key_columns = Vec::with_capacity(r.capped_capacity(key_count));
    for _ in 0..key_count {
        key_columns.push(read_index_key_column(r)?);
    }
    let inc_count = read_bounded_u32_count(r, "index include column count", MAX_COLLECTION_ITEMS)?;
    let mut include_columns = Vec::with_capacity(r.capped_capacity(inc_count));
    for _ in 0..inc_count {
        include_columns.push(ColumnId::new(r.read_u64()?));
    }
    let hnsw_options_tag = r.read_u8()?;
    let hnsw_options = match hnsw_options_tag {
        0 => None,
        1 | 2 => {
            let m = r.read_u32()?;
            let ef_construction = r.read_u32()?;
            let distance_metric = read_stored_metric(r)?;
            let quantization = read_stored_quantization(r)?;
            // Tag 2 added the `prenormalised` flag at the end; tag 1 entries
            // do not carry the flag so we default it to `false`.
            let prenormalised = if hnsw_options_tag == 2 {
                r.read_bool()?
            } else {
                false
            };
            Some(aiondb_storage_api::HnswStorageOptions {
                m,
                ef_construction,
                distance_metric,
                quantization,
                prenormalised,
            })
        }
        other => {
            return Err(DbError::internal(format!(
                "WAL: unknown hnsw_options tag {other}"
            )));
        }
    };
    // Backward compatibility: older WAL entries did not encode these flags.
    let gin = if r.remaining() > 0 {
        r.read_bool()?
    } else {
        false
    };
    let nulls_not_distinct = if r.remaining() > 0 {
        r.read_bool()?
    } else {
        false
    };
    Ok(IndexStorageDescriptor {
        index_id,
        table_id,
        unique,
        nulls_not_distinct,
        gin,
        key_columns,
        include_columns,
        hnsw_options,
        // IVF-flat descriptor encoding lives outside this codec for v0.3;
        // legacy WAL records never carried it, so reconstruction always
        // yields None. New IVF indexes only persist via the in-memory
        // engine state until the WAL codec is extended.
        ivf_flat_options: None,
    })
}

fn stored_metric_tag(metric: aiondb_storage_api::StoredVectorMetric) -> u8 {
    match metric {
        aiondb_storage_api::StoredVectorMetric::L2 => 0,
        aiondb_storage_api::StoredVectorMetric::Cosine => 1,
        aiondb_storage_api::StoredVectorMetric::InnerProduct => 2,
        aiondb_storage_api::StoredVectorMetric::Manhattan => 3,
    }
}

fn read_stored_metric(
    r: &mut BinaryReader<'_>,
) -> DbResult<aiondb_storage_api::StoredVectorMetric> {
    use aiondb_storage_api::StoredVectorMetric;
    match r.read_u8()? {
        0 => Ok(StoredVectorMetric::L2),
        1 => Ok(StoredVectorMetric::Cosine),
        2 => Ok(StoredVectorMetric::InnerProduct),
        3 => Ok(StoredVectorMetric::Manhattan),
        other => Err(DbError::internal(format!(
            "WAL: unknown stored vector metric tag {other}"
        ))),
    }
}

fn stored_quantization_tag(kind: aiondb_storage_api::StoredQuantizationKind) -> u8 {
    match kind {
        aiondb_storage_api::StoredQuantizationKind::None => 0,
        aiondb_storage_api::StoredQuantizationKind::Scalar => 1,
        aiondb_storage_api::StoredQuantizationKind::Binary => 2,
        aiondb_storage_api::StoredQuantizationKind::Product => 3,
    }
}

fn read_stored_quantization(
    r: &mut BinaryReader<'_>,
) -> DbResult<aiondb_storage_api::StoredQuantizationKind> {
    use aiondb_storage_api::StoredQuantizationKind;
    match r.read_u8()? {
        0 => Ok(StoredQuantizationKind::None),
        1 => Ok(StoredQuantizationKind::Scalar),
        2 => Ok(StoredQuantizationKind::Binary),
        3 => Ok(StoredQuantizationKind::Product),
        other => Err(DbError::internal(format!(
            "WAL: unknown stored quantization tag {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// IsolationLevel encoding/decoding
// ---------------------------------------------------------------------------

fn write_isolation(w: &mut BinaryWriter, iso: &IsolationLevel) {
    match iso {
        IsolationLevel::ReadCommitted => w.write_u8(0),
        IsolationLevel::SnapshotIsolation => w.write_u8(1),
        IsolationLevel::Serializable => w.write_u8(2),
        _ => w.write_u8(0xFF), // Unknown isolation level sentinel.
    }
}

fn read_isolation(r: &mut BinaryReader<'_>) -> DbResult<IsolationLevel> {
    let tag = r.read_u8()?;
    match tag {
        0 => Ok(IsolationLevel::ReadCommitted),
        1 => Ok(IsolationLevel::SnapshotIsolation),
        2 => Ok(IsolationLevel::Serializable),
        _ => Err(DbError::internal(format!(
            "WAL: unknown isolation tag {tag}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// WalRecord payload encoding/decoding
// ---------------------------------------------------------------------------

fn write_record_payload(w: &mut BinaryWriter, record: &WalRecord) -> DbResult<()> {
    w.write_u8(record.tag());
    match record {
        WalRecord::BeginTxn { txn_id, isolation } => {
            w.write_u64(txn_id.get());
            write_isolation(w, isolation);
        }
        WalRecord::CommitTxn { txn_id, commit_ts } => {
            w.write_u64(txn_id.get());
            w.write_u64(*commit_ts);
        }
        WalRecord::AbortTxn { txn_id } => {
            w.write_u64(txn_id.get());
        }
        WalRecord::InsertRow {
            txn_id,
            table_id,
            tuple_id,
            row,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(tuple_id.get());
            write_row(w, row)?;
        }
        WalRecord::DeleteRow {
            txn_id,
            table_id,
            tuple_id,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(tuple_id.get());
        }
        WalRecord::UpdateRow {
            txn_id,
            table_id,
            old_tuple_id,
            new_tuple_id,
            row,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(old_tuple_id.get());
            w.write_u64(new_tuple_id.get());
            write_row(w, row)?;
        }
        WalRecord::AutocommitInsertRow {
            txn_id,
            table_id,
            tuple_id,
            row,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(tuple_id.get());
            write_row(w, row)?;
        }
        WalRecord::AutocommitDeleteRow {
            txn_id,
            table_id,
            tuple_id,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(tuple_id.get());
        }
        WalRecord::AutocommitUpdateRow {
            txn_id,
            table_id,
            old_tuple_id,
            new_tuple_id,
            row,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(old_tuple_id.get());
            w.write_u64(new_tuple_id.get());
            write_row(w, row)?;
        }
        WalRecord::CreateTable { txn_id, descriptor } => {
            w.write_u64(txn_id.get());
            write_table_descriptor(w, descriptor)?;
        }
        WalRecord::DropTable { txn_id, table_id } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
        }
        WalRecord::CreateIndex { txn_id, descriptor } => {
            w.write_u64(txn_id.get());
            write_index_descriptor(w, descriptor)?;
        }
        WalRecord::DropIndex { txn_id, index_id } => {
            w.write_u64(txn_id.get());
            w.write_u64(index_id.get());
        }
        WalRecord::AlterTable { txn_id, descriptor } => {
            w.write_u64(txn_id.get());
            write_table_descriptor(w, descriptor)?;
        }
        WalRecord::Checkpoint { last_committed_lsn } => {
            w.write_u64(last_committed_lsn.get());
        }
        WalRecord::UpdateStatistics {
            table_id,
            row_count,
            total_bytes,
            dead_row_count,
            column_stats,
        } => {
            w.write_u64(table_id.get());
            w.write_u64(*row_count);
            w.write_u64(*total_bytes);
            w.write_u64(*dead_row_count);
            write_bounded_u32_count(
                w,
                "column statistics count",
                column_stats.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (col_id, ndistinct, null_fraction, avg_width) in column_stats {
                w.write_u64(col_id.get());
                w.write_f64(*ndistinct);
                w.write_f64(*null_fraction);
                w.write_u32(*avg_width);
            }
        }
        WalRecord::FullPageImage {
            relation_id,
            page_number,
            page_data,
        } => {
            if page_data.len() > MAX_FULL_PAGE_IMAGE_BYTES {
                return Err(DbError::internal(format!(
                    "WAL: full page image payload {} exceeds limit {}",
                    page_data.len(),
                    MAX_FULL_PAGE_IMAGE_BYTES
                )));
            }
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            w.write_bytes(page_data)?;
        }
        WalRecord::FullPageImageBatch { relation_id, pages } => {
            w.write_u64(relation_id.get());
            write_bounded_u32_count(
                w,
                "full page image count",
                pages.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (page_number, page_data) in pages {
                if page_data.len() > MAX_FULL_PAGE_IMAGE_BYTES {
                    return Err(DbError::internal(format!(
                        "WAL: full page image payload {} exceeds limit {}",
                        page_data.len(),
                        MAX_FULL_PAGE_IMAGE_BYTES
                    )));
                }
                w.write_u64(*page_number);
                w.write_bytes(page_data)?;
            }
        }
        WalRecord::PagePatch {
            relation_id,
            page_number,
            segments,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            write_bounded_u32_count(
                w,
                "page patch segment count",
                segments.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (offset, data) in segments {
                w.write_u16(*offset);
                w.write_bytes(data)?;
            }
        }
        WalRecord::PagePatchBatch {
            relation_id,
            patches,
        } => {
            w.write_u64(relation_id.get());
            write_bounded_u32_count(w, "page patch count", patches.len(), MAX_COLLECTION_ITEMS)?;
            for (page_number, segments) in patches {
                w.write_u64(*page_number);
                write_bounded_u32_count(
                    w,
                    "page patch segment count",
                    segments.len(),
                    MAX_COLLECTION_ITEMS,
                )?;
                for (offset, data) in segments {
                    w.write_u16(*offset);
                    w.write_bytes(data)?;
                }
            }
        }
        WalRecord::PageSetU64Batch {
            relation_id,
            updates,
        } => {
            w.write_u64(relation_id.get());
            write_bounded_u32_count(
                w,
                "page set-u64 update count",
                updates.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (page_number, offset, value) in updates {
                w.write_u64(*page_number);
                w.write_u16(*offset);
                w.write_u64(*value);
            }
        }
        WalRecord::DiskBtreeMetaUpdate {
            relation_id,
            root_page,
            height,
            page_count,
            free_list_head,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*root_page);
            w.write_u32(*height);
            w.write_u64(*page_count);
            w.write_u64(*free_list_head);
        }
        WalRecord::DiskBtreeLeafInsert {
            relation_id,
            page_number,
            key,
            value,
        }
        | WalRecord::DiskBtreeLeafDelete {
            relation_id,
            page_number,
            key,
            value,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            w.write_u64(*key);
            w.write_u64(*value);
        }
        WalRecord::DiskBtreeLeafSplit {
            relation_id,
            left_page,
            right_page,
            old_right_sibling,
            separator,
            left_entries,
            right_entries,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*old_right_sibling);
            w.write_u64(*separator);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            write_bounded_u32_count(
                w,
                "right btree entry count",
                right_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in right_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
        }
        WalRecord::DiskBtreeInternalInsert {
            relation_id,
            page_number,
            separator,
            child_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            w.write_u64(*separator);
            w.write_u64(*child_page);
        }
        WalRecord::DiskBtreeInternalSplit {
            relation_id,
            left_page,
            right_page,
            promoted_separator,
            left_first_child,
            right_first_child,
            left_entries,
            right_entries,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*promoted_separator);
            w.write_u64(*left_first_child);
            w.write_u64(*right_first_child);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            write_bounded_u32_count(
                w,
                "right btree entry count",
                right_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in right_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
        }
        WalRecord::DiskBtreeRootGrow {
            relation_id,
            page_number,
            first_child,
            separator,
            right_child,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            w.write_u64(*first_child);
            w.write_u64(*separator);
            w.write_u64(*right_child);
        }
        WalRecord::DiskBtreeInternalDelete {
            relation_id,
            page_number,
            separator,
            child_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*page_number);
            w.write_u64(*separator);
            w.write_u64(*child_page);
        }
        WalRecord::DiskBtreeLeafRedistribute {
            relation_id,
            left_page,
            right_page,
            parent_page,
            parent_slot,
            parent_first_child,
            left_entries,
            right_entries,
            right_right_sibling,
            new_separator,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*parent_page);
            w.write_u32(*parent_slot);
            w.write_u64(*parent_first_child);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            write_bounded_u32_count(
                w,
                "right btree entry count",
                right_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in right_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            w.write_u64(*right_right_sibling);
            w.write_u64(*new_separator);
        }
        WalRecord::DiskBtreeInternalRedistribute {
            relation_id,
            left_page,
            right_page,
            parent_page,
            parent_slot,
            parent_first_child,
            left_first_child,
            right_first_child,
            left_entries,
            right_entries,
            new_separator,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*parent_page);
            w.write_u32(*parent_slot);
            w.write_u64(*parent_first_child);
            w.write_u64(*left_first_child);
            w.write_u64(*right_first_child);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            write_bounded_u32_count(
                w,
                "right btree entry count",
                right_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in right_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            w.write_u64(*new_separator);
        }
        WalRecord::DiskBtreeLeafMerge {
            relation_id,
            left_page,
            right_page,
            parent_page,
            parent_first_child,
            removed_separator,
            left_entries,
            new_right_sibling,
            next_free_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*parent_page);
            w.write_u64(*parent_first_child);
            w.write_u64(*removed_separator);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            w.write_u64(*new_right_sibling);
            w.write_u64(*next_free_page);
        }
        WalRecord::DiskBtreeInternalMerge {
            relation_id,
            left_page,
            right_page,
            parent_page,
            parent_first_child,
            removed_separator,
            left_first_child,
            left_entries,
            next_free_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*left_page);
            w.write_u64(*right_page);
            w.write_u64(*parent_page);
            w.write_u64(*parent_first_child);
            w.write_u64(*removed_separator);
            w.write_u64(*left_first_child);
            write_bounded_u32_count(
                w,
                "left btree entry count",
                left_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in left_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            w.write_u64(*next_free_page);
        }
        WalRecord::DiskBtreeRootShrinkLeaf {
            relation_id,
            root_page,
            root_entries,
            right_sibling,
            freed_pages,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*root_page);
            write_bounded_u32_count(
                w,
                "root btree entry count",
                root_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in root_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            w.write_u64(*right_sibling);
            write_bounded_u32_count(
                w,
                "freed page count",
                freed_pages.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (page_no, next_free) in freed_pages {
                w.write_u64(*page_no);
                w.write_u64(*next_free);
            }
        }
        WalRecord::DiskBtreeRootShrinkInternal {
            relation_id,
            root_page,
            root_first_child,
            root_entries,
            freed_pages,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*root_page);
            w.write_u64(*root_first_child);
            write_bounded_u32_count(
                w,
                "root btree entry count",
                root_entries.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (key, value) in root_entries {
                w.write_u64(*key);
                w.write_u64(*value);
            }
            write_bounded_u32_count(
                w,
                "freed page count",
                freed_pages.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (page_no, next_free) in freed_pages {
                w.write_u64(*page_no);
                w.write_u64(*next_free);
            }
        }
        WalRecord::DiskBtreeInternalCollapse {
            relation_id,
            parent_page,
            parent_slot,
            parent_first_child,
            replacement_child,
            removed_page,
            next_free_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*parent_page);
            w.write_u32(*parent_slot);
            w.write_u64(*parent_first_child);
            w.write_u64(*replacement_child);
            w.write_u64(*removed_page);
            w.write_u64(*next_free_page);
        }
        WalRecord::DiskBtreeRootPromoteSingleChild {
            relation_id,
            new_root_page,
            removed_root_page,
            next_free_page,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*new_root_page);
            w.write_u64(*removed_root_page);
            w.write_u64(*next_free_page);
        }
        WalRecord::DiskBtreeRootPromoteCollapsedChain {
            relation_id,
            new_root_page,
            freed_pages,
        } => {
            w.write_u64(relation_id.get());
            w.write_u64(*new_root_page);
            write_bounded_u32_count(
                w,
                "freed page count",
                freed_pages.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (page_no, next_free) in freed_pages {
                w.write_u64(*page_no);
                w.write_u64(*next_free);
            }
        }
        WalRecord::DiskBtreeInternalCollapseChain { relation_id, steps } => {
            w.write_u64(relation_id.get());
            write_bounded_u32_count(
                w,
                "btree collapse step count",
                steps.len(),
                MAX_COLLECTION_ITEMS,
            )?;
            for (
                parent_page,
                parent_slot,
                parent_first_child,
                replacement_child,
                removed_page,
                next_free_page,
            ) in steps
            {
                w.write_u64(*parent_page);
                w.write_u32(*parent_slot);
                w.write_u64(*parent_first_child);
                w.write_u64(*replacement_child);
                w.write_u64(*removed_page);
                w.write_u64(*next_free_page);
            }
        }
        WalRecord::PagedRowRef {
            txn_id,
            table_id,
            tuple_id,
        } => {
            w.write_u64(txn_id.get());
            w.write_u64(table_id.get());
            w.write_u64(tuple_id.get());
        }
        // Adjacency index records (tags 30..=32).
        WalRecord::RegisterEdgeTable {
            table_id,
            source_col,
            target_col,
        } => {
            w.write_u64(table_id.get());
            w.write_u64(usize_to_u64_saturating(*source_col));
            w.write_u64(usize_to_u64_saturating(*target_col));
        }
        WalRecord::AdjacencyInsert {
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
        } => {
            w.write_u64(table_id.get());
            write_value(w, source_id)?;
            write_value(w, target_id)?;
            w.write_u64(edge_tuple_id.get());
        }
        WalRecord::AdjacencyRemove {
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
        } => {
            w.write_u64(table_id.get());
            write_value(w, source_id)?;
            write_value(w, target_id)?;
            w.write_u64(edge_tuple_id.get());
        }
        // Catalog DDL records are handled by the catalog submodule.
        _ => catalog::write_catalog_payload(w, record)?,
    }
    Ok(())
}

fn read_record_payload(r: &mut BinaryReader<'_>) -> DbResult<WalRecord> {
    let tag = r.read_u8()?;
    match tag {
        0 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let isolation = read_isolation(r)?;
            Ok(WalRecord::BeginTxn { txn_id, isolation })
        }
        1 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let commit_ts = r.read_u64()?;
            Ok(WalRecord::CommitTxn { txn_id, commit_ts })
        }
        2 => {
            let txn_id = TxnId::new(r.read_u64()?);
            Ok(WalRecord::AbortTxn { txn_id })
        }
        3 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let tuple_id = TupleId::new(r.read_u64()?);
            let row = read_row(r)?;
            Ok(WalRecord::InsertRow {
                txn_id,
                table_id,
                tuple_id,
                row,
            })
        }
        4 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let tuple_id = TupleId::new(r.read_u64()?);
            Ok(WalRecord::DeleteRow {
                txn_id,
                table_id,
                tuple_id,
            })
        }
        5 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let old_tuple_id = TupleId::new(r.read_u64()?);
            let new_tuple_id = TupleId::new(r.read_u64()?);
            let row = read_row(r)?;
            Ok(WalRecord::UpdateRow {
                txn_id,
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
            })
        }
        68 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let tuple_id = TupleId::new(r.read_u64()?);
            let row = read_row(r)?;
            Ok(WalRecord::AutocommitInsertRow {
                txn_id,
                table_id,
                tuple_id,
                row,
            })
        }
        69 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let tuple_id = TupleId::new(r.read_u64()?);
            Ok(WalRecord::AutocommitDeleteRow {
                txn_id,
                table_id,
                tuple_id,
            })
        }
        70 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let old_tuple_id = TupleId::new(r.read_u64()?);
            let new_tuple_id = TupleId::new(r.read_u64()?);
            let row = read_row(r)?;
            Ok(WalRecord::AutocommitUpdateRow {
                txn_id,
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
            })
        }
        6 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let descriptor = read_table_descriptor(r)?;
            Ok(WalRecord::CreateTable { txn_id, descriptor })
        }
        7 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            Ok(WalRecord::DropTable { txn_id, table_id })
        }
        8 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let descriptor = read_index_descriptor(r)?;
            Ok(WalRecord::CreateIndex { txn_id, descriptor })
        }
        9 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let index_id = IndexId::new(r.read_u64()?);
            Ok(WalRecord::DropIndex { txn_id, index_id })
        }
        10 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let descriptor = read_table_descriptor(r)?;
            Ok(WalRecord::AlterTable { txn_id, descriptor })
        }
        11 => {
            let lsn = Lsn::new(r.read_u64()?);
            Ok(WalRecord::Checkpoint {
                last_committed_lsn: lsn,
            })
        }
        12 => {
            let table_id = RelationId::new(r.read_u64()?);
            let row_count = r.read_u64()?;
            let total_bytes = r.read_u64()?;
            let dead_row_count = r.read_u64()?;
            let col_count =
                read_bounded_u32_count(r, "statistics column count", MAX_COLLECTION_ITEMS)?;
            let mut column_stats = Vec::with_capacity(r.capped_capacity(col_count));
            for _ in 0..col_count {
                let col_id = ColumnId::new(r.read_u64()?);
                let ndistinct = r.read_f64()?;
                let null_fraction = r.read_f64()?;
                let avg_width = r.read_u32()?;
                column_stats.push((col_id, ndistinct, null_fraction, avg_width));
            }
            Ok(WalRecord::UpdateStatistics {
                table_id,
                row_count,
                total_bytes,
                dead_row_count,
                column_stats,
            })
        }
        44 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let page_data = r.read_bytes()?;
            if page_data.len() > MAX_FULL_PAGE_IMAGE_BYTES {
                return Err(DbError::internal(format!(
                    "WAL: full page image payload {} exceeds limit {}",
                    page_data.len(),
                    MAX_FULL_PAGE_IMAGE_BYTES
                )));
            }
            Ok(WalRecord::FullPageImage {
                relation_id,
                page_number,
                page_data,
            })
        }
        45 => {
            let txn_id = TxnId::new(r.read_u64()?);
            let table_id = RelationId::new(r.read_u64()?);
            let tuple_id = TupleId::new(r.read_u64()?);
            Ok(WalRecord::PagedRowRef {
                txn_id,
                table_id,
                tuple_id,
            })
        }
        46 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let count = read_bounded_u32_count(
                r,
                "full page image batch page count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut pages = Vec::with_capacity(r.capped_capacity(count));
            for _ in 0..count {
                let page_number = r.read_u64()?;
                let page_data = r.read_bytes()?;
                if page_data.len() > MAX_FULL_PAGE_IMAGE_BYTES {
                    return Err(DbError::internal(format!(
                        "WAL: full page image payload {} exceeds limit {}",
                        page_data.len(),
                        MAX_FULL_PAGE_IMAGE_BYTES
                    )));
                }
                pages.push((page_number, page_data));
            }
            Ok(WalRecord::FullPageImageBatch { relation_id, pages })
        }
        47 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let count =
                read_bounded_u32_count(r, "page patch segment count", MAX_COLLECTION_ITEMS)?;
            let mut segments = Vec::with_capacity(r.capped_capacity(count));
            for _ in 0..count {
                let offset = r.read_u16()?;
                let data = r.read_bytes()?;
                segments.push((offset, data));
            }
            Ok(WalRecord::PagePatch {
                relation_id,
                page_number,
                segments,
            })
        }
        48 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let patch_count =
                read_bounded_u32_count(r, "page patch batch count", MAX_COLLECTION_ITEMS)?;
            let mut patches = Vec::with_capacity(r.capped_capacity(patch_count));
            for _ in 0..patch_count {
                let page_number = r.read_u64()?;
                let count =
                    read_bounded_u32_count(r, "page patch segment count", MAX_COLLECTION_ITEMS)?;
                let mut segments = Vec::with_capacity(r.capped_capacity(count));
                for _ in 0..count {
                    let offset = r.read_u16()?;
                    let data = r.read_bytes()?;
                    segments.push((offset, data));
                }
                patches.push((page_number, segments));
            }
            Ok(WalRecord::PagePatchBatch {
                relation_id,
                patches,
            })
        }
        49 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let count =
                read_bounded_u32_count(r, "page u64 update batch count", MAX_COLLECTION_ITEMS)?;
            let mut updates = Vec::with_capacity(r.capped_capacity(count));
            for _ in 0..count {
                let page_number = r.read_u64()?;
                let offset = r.read_u16()?;
                let value = r.read_u64()?;
                updates.push((page_number, offset, value));
            }
            Ok(WalRecord::PageSetU64Batch {
                relation_id,
                updates,
            })
        }
        50 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let root_page = r.read_u64()?;
            let height = r.read_u32()?;
            let page_count = r.read_u64()?;
            let free_list_head = r.read_u64()?;
            Ok(WalRecord::DiskBtreeMetaUpdate {
                relation_id,
                root_page,
                height,
                page_count,
                free_list_head,
            })
        }
        51 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let key = r.read_u64()?;
            let value = r.read_u64()?;
            Ok(WalRecord::DiskBtreeLeafInsert {
                relation_id,
                page_number,
                key,
                value,
            })
        }
        52 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let key = r.read_u64()?;
            let value = r.read_u64()?;
            Ok(WalRecord::DiskBtreeLeafDelete {
                relation_id,
                page_number,
                key,
                value,
            })
        }
        53 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let old_right_sibling = r.read_u64()?;
            let separator = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left leaf entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_count = read_bounded_u32_count(
                r,
                "disk btree right leaf entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut right_entries = Vec::with_capacity(r.capped_capacity(right_count));
            for _ in 0..right_count {
                right_entries.push((r.read_u64()?, r.read_u64()?));
            }
            Ok(WalRecord::DiskBtreeLeafSplit {
                relation_id,
                left_page,
                right_page,
                old_right_sibling,
                separator,
                left_entries,
                right_entries,
            })
        }
        54 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let separator = r.read_u64()?;
            let child_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeInternalInsert {
                relation_id,
                page_number,
                separator,
                child_page,
            })
        }
        55 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let promoted_separator = r.read_u64()?;
            let left_first_child = r.read_u64()?;
            let right_first_child = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left internal entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_count = read_bounded_u32_count(
                r,
                "disk btree right internal entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut right_entries = Vec::with_capacity(r.capped_capacity(right_count));
            for _ in 0..right_count {
                right_entries.push((r.read_u64()?, r.read_u64()?));
            }
            Ok(WalRecord::DiskBtreeInternalSplit {
                relation_id,
                left_page,
                right_page,
                promoted_separator,
                left_first_child,
                right_first_child,
                left_entries,
                right_entries,
            })
        }
        56 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let first_child = r.read_u64()?;
            let separator = r.read_u64()?;
            let right_child = r.read_u64()?;
            Ok(WalRecord::DiskBtreeRootGrow {
                relation_id,
                page_number,
                first_child,
                separator,
                right_child,
            })
        }
        57 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let page_number = r.read_u64()?;
            let separator = r.read_u64()?;
            let child_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeInternalDelete {
                relation_id,
                page_number,
                separator,
                child_page,
            })
        }
        58 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let parent_page = r.read_u64()?;
            let parent_slot = r.read_u32()?;
            let parent_first_child = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left leaf redistribution entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_count = read_bounded_u32_count(
                r,
                "disk btree right leaf redistribution entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut right_entries = Vec::with_capacity(r.capped_capacity(right_count));
            for _ in 0..right_count {
                right_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_right_sibling = r.read_u64()?;
            let new_separator = r.read_u64()?;
            Ok(WalRecord::DiskBtreeLeafRedistribute {
                relation_id,
                left_page,
                right_page,
                parent_page,
                parent_slot,
                parent_first_child,
                left_entries,
                right_entries,
                right_right_sibling,
                new_separator,
            })
        }
        59 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let parent_page = r.read_u64()?;
            let parent_slot = r.read_u32()?;
            let parent_first_child = r.read_u64()?;
            let left_first_child = r.read_u64()?;
            let right_first_child = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left internal redistribution entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_count = read_bounded_u32_count(
                r,
                "disk btree right internal redistribution entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut right_entries = Vec::with_capacity(r.capped_capacity(right_count));
            for _ in 0..right_count {
                right_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let new_separator = r.read_u64()?;
            Ok(WalRecord::DiskBtreeInternalRedistribute {
                relation_id,
                left_page,
                right_page,
                parent_page,
                parent_slot,
                parent_first_child,
                left_first_child,
                right_first_child,
                left_entries,
                right_entries,
                new_separator,
            })
        }
        60 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let parent_page = r.read_u64()?;
            let parent_first_child = r.read_u64()?;
            let removed_separator = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left leaf merge entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let new_right_sibling = r.read_u64()?;
            let next_free_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeLeafMerge {
                relation_id,
                left_page,
                right_page,
                parent_page,
                parent_first_child,
                removed_separator,
                left_entries,
                new_right_sibling,
                next_free_page,
            })
        }
        61 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let left_page = r.read_u64()?;
            let right_page = r.read_u64()?;
            let parent_page = r.read_u64()?;
            let parent_first_child = r.read_u64()?;
            let removed_separator = r.read_u64()?;
            let left_first_child = r.read_u64()?;
            let left_count = read_bounded_u32_count(
                r,
                "disk btree left internal merge entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut left_entries = Vec::with_capacity(r.capped_capacity(left_count));
            for _ in 0..left_count {
                left_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let next_free_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeInternalMerge {
                relation_id,
                left_page,
                right_page,
                parent_page,
                parent_first_child,
                removed_separator,
                left_first_child,
                left_entries,
                next_free_page,
            })
        }
        62 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let root_page = r.read_u64()?;
            let root_count = read_bounded_u32_count(
                r,
                "disk btree root shrink leaf entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut root_entries = Vec::with_capacity(r.capped_capacity(root_count));
            for _ in 0..root_count {
                root_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let right_sibling = r.read_u64()?;
            let freed_count = read_bounded_u32_count(
                r,
                "disk btree root shrink leaf free-page count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut freed_pages = Vec::with_capacity(r.capped_capacity(freed_count));
            for _ in 0..freed_count {
                freed_pages.push((r.read_u64()?, r.read_u64()?));
            }
            Ok(WalRecord::DiskBtreeRootShrinkLeaf {
                relation_id,
                root_page,
                root_entries,
                right_sibling,
                freed_pages,
            })
        }
        63 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let root_page = r.read_u64()?;
            let root_first_child = r.read_u64()?;
            let root_count = read_bounded_u32_count(
                r,
                "disk btree root shrink internal entry count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut root_entries = Vec::with_capacity(r.capped_capacity(root_count));
            for _ in 0..root_count {
                root_entries.push((r.read_u64()?, r.read_u64()?));
            }
            let freed_count = read_bounded_u32_count(
                r,
                "disk btree root shrink internal free-page count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut freed_pages = Vec::with_capacity(r.capped_capacity(freed_count));
            for _ in 0..freed_count {
                freed_pages.push((r.read_u64()?, r.read_u64()?));
            }
            Ok(WalRecord::DiskBtreeRootShrinkInternal {
                relation_id,
                root_page,
                root_first_child,
                root_entries,
                freed_pages,
            })
        }
        64 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let parent_page = r.read_u64()?;
            let parent_slot = r.read_u32()?;
            let parent_first_child = r.read_u64()?;
            let replacement_child = r.read_u64()?;
            let removed_page = r.read_u64()?;
            let next_free_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeInternalCollapse {
                relation_id,
                parent_page,
                parent_slot,
                parent_first_child,
                replacement_child,
                removed_page,
                next_free_page,
            })
        }
        65 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let new_root_page = r.read_u64()?;
            let removed_root_page = r.read_u64()?;
            let next_free_page = r.read_u64()?;
            Ok(WalRecord::DiskBtreeRootPromoteSingleChild {
                relation_id,
                new_root_page,
                removed_root_page,
                next_free_page,
            })
        }
        66 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let new_root_page = r.read_u64()?;
            let freed_count = read_bounded_u32_count(
                r,
                "disk btree root promote collapsed-chain free-page count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut freed_pages = Vec::with_capacity(r.capped_capacity(freed_count));
            for _ in 0..freed_count {
                freed_pages.push((r.read_u64()?, r.read_u64()?));
            }
            Ok(WalRecord::DiskBtreeRootPromoteCollapsedChain {
                relation_id,
                new_root_page,
                freed_pages,
            })
        }
        67 => {
            let relation_id = RelationId::new(r.read_u64()?);
            let step_count = read_bounded_u32_count(
                r,
                "disk btree internal collapse-chain step count",
                MAX_COLLECTION_ITEMS,
            )?;
            let mut steps = Vec::with_capacity(r.capped_capacity(step_count));
            for _ in 0..step_count {
                steps.push((
                    r.read_u64()?,
                    r.read_u32()?,
                    r.read_u64()?,
                    r.read_u64()?,
                    r.read_u64()?,
                    r.read_u64()?,
                ));
            }
            Ok(WalRecord::DiskBtreeInternalCollapseChain { relation_id, steps })
        }
        // Catalog DDL records.
        13..=29 | 33..=43 | 71..=85 => catalog::read_catalog_payload(tag, r),
        // Adjacency index records (tags 30..=32).
        30 => {
            let table_id = RelationId::new(r.read_u64()?);
            let source_col =
                read_bounded_u64_usize(r, "edge source column ordinal", MAX_COLUMN_ORDINAL)?;
            let target_col =
                read_bounded_u64_usize(r, "edge target column ordinal", MAX_COLUMN_ORDINAL)?;
            Ok(WalRecord::RegisterEdgeTable {
                table_id,
                source_col,
                target_col,
            })
        }
        31 => {
            let table_id = RelationId::new(r.read_u64()?);
            let source_id = read_value(r)?;
            let target_id = read_value(r)?;
            let edge_tuple_id = TupleId::new(r.read_u64()?);
            Ok(WalRecord::AdjacencyInsert {
                table_id,
                source_id,
                target_id,
                edge_tuple_id,
            })
        }
        32 => {
            let table_id = RelationId::new(r.read_u64()?);
            let source_id = read_value(r)?;
            let target_id = read_value(r)?;
            let edge_tuple_id = TupleId::new(r.read_u64()?);
            Ok(WalRecord::AdjacencyRemove {
                table_id,
                source_id,
                target_id,
                edge_tuple_id,
            })
        }
        _ => Err(DbError::internal(format!("WAL: unknown record tag {tag}"))),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a storage row using the same self-describing binary value tags as the
/// WAL row payload format.
pub fn encode_row(row: &Row) -> DbResult<Vec<u8>> {
    let mut writer = BinaryWriter::new();
    write_row(&mut writer, row)?;
    Ok(writer.into_bytes())
}

/// Decode a storage row previously encoded with [`encode_row`].
///
/// # Errors
/// Returns an error if the payload is truncated, malformed, or contains
/// trailing bytes after a complete row.
pub fn decode_row(data: &[u8]) -> DbResult<Row> {
    let mut reader = BinaryReader::new(data);
    let row = read_row(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(DbError::internal("WAL row payload has trailing bytes"));
    }
    Ok(row)
}

fn write_entry_with_checksum(lsn: Lsn, payload_bytes: &[u8]) -> DbResult<Vec<u8>> {
    // payload_len = 8 (lsn) + payload.len() + 4 (checksum)
    let total_len = 8usize + payload_bytes.len() + 4;
    let payload_len: u32 = total_len.try_into().map_err(|_| {
        DbError::internal(format!(
            "WAL entry payload too large: {total_len} bytes exceeds u32::MAX"
        ))
    })?;

    // Build the checksummed region: lsn bytes + payload bytes.
    let mut checksum_input = Vec::with_capacity(8 + payload_bytes.len());
    checksum_input.extend_from_slice(&lsn.get().to_le_bytes());
    checksum_input.extend_from_slice(payload_bytes);
    let checksum = compute_crc32c(&checksum_input);

    // Write the full on-disk entry.
    let payload_len_usize = usize::try_from(payload_len)
        .map_err(|_| DbError::internal("WAL entry payload length overflow"))?;
    let mut out = BinaryWriter::with_capacity(4 + payload_len_usize);
    out.write_u32(payload_len);
    out.write_u64(lsn.get());
    out.write_raw(payload_bytes);
    out.write_u32(checksum);

    Ok(out.into_bytes())
}

fn encode_uncompressed_entry(entry: &WalEntry) -> DbResult<Vec<u8>> {
    let mut payload_writer = BinaryWriter::new();
    write_record_payload(&mut payload_writer, &entry.record)?;
    let payload_bytes = payload_writer.into_bytes();
    write_entry_with_checksum(entry.lsn, &payload_bytes)
}

/// Maximum decoded payload size for compressed WAL records.
const MAX_COMPRESSED_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Maximum decompression ratio (decoded / encoded) accepted at replay. Caps a
/// forged tiny-on-disk record from forcing a 64 MiB allocation. Legitimate
/// FullPageImageBatch records of mostly-zero pages can compress at 100x+
/// ratios with lz4, so the cap stays loose; the realistic attack vector is a
/// record claiming 64 MiB original size from <2 KiB encoded data, which is
/// still blocked at ratio > 4096.
const MAX_WAL_DECOMPRESSION_RATIO: usize = 4096;
/// Default zstd level used for WAL entry compression.
const WAL_ZSTD_LEVEL: i32 = 1;

fn compress_record_payload(
    payload: &[u8],
    compression: WalCompression,
) -> DbResult<Option<Vec<u8>>> {
    let compressed = match compression {
        WalCompression::None => return Ok(None),
        WalCompression::Lz4 => lz4_flex::block::compress(payload),
        WalCompression::Zstd => zstd::bulk::compress(payload, WAL_ZSTD_LEVEL)
            .map_err(|error| DbError::internal(format!("WAL: zstd compression failed: {error}")))?,
    };

    // v2 envelope adds 7 bytes in front of the compressed payload
    // (marker + format_version + compression + original_len).
    if compressed.len().saturating_add(7) >= payload.len() {
        return Ok(None);
    }

    Ok(Some(compressed))
}

fn encode_framed_payload_v3(
    original_payload: &[u8],
    compression_tag: u8,
    encoded_payload: &[u8],
    prev_lsn: Lsn,
    database_id: u32,
) -> DbResult<Vec<u8>> {
    let original_len: u32 = original_payload.len().try_into().map_err(|_| {
        DbError::internal(format!(
            "WAL payload too large: {} bytes exceeds u32::MAX",
            original_payload.len()
        ))
    })?;

    // marker + format + compression + prev_lsn + database_id + original_len
    const V3_HEADER_BYTES: usize = 1 + 1 + 1 + 8 + 4 + 4;
    let mut framed_payload = BinaryWriter::with_capacity(V3_HEADER_BYTES + encoded_payload.len());
    framed_payload.write_u8(ENTRY_V2_MARKER);
    framed_payload.write_u8(ENTRY_FRAMED_FORMAT_VERSION_V3);
    framed_payload.write_u8(compression_tag);
    framed_payload.write_u64(prev_lsn.get());
    framed_payload.write_u32(database_id);
    framed_payload.write_u32(original_len);
    framed_payload.write_raw(encoded_payload);
    Ok(framed_payload.into_bytes())
}

fn compression_tag(compression: WalCompression) -> u8 {
    match compression {
        WalCompression::None => ENTRY_COMPRESSION_NONE,
        WalCompression::Lz4 => ENTRY_COMPRESSION_LZ4,
        WalCompression::Zstd => ENTRY_COMPRESSION_ZSTD,
    }
}

pub fn prepare_record_with_compression(
    record: &WalRecord,
    compression: WalCompression,
) -> DbResult<PreparedWalRecord> {
    let mut payload_writer = BinaryWriter::new();
    write_record_payload(&mut payload_writer, record)?;
    let payload_bytes = payload_writer.into_bytes();
    let compressed_payload = compress_record_payload(&payload_bytes, compression)?;
    Ok(PreparedWalRecord {
        payload_bytes,
        compressed_payload,
        compression_tag: compression_tag(compression),
    })
}

pub fn encode_prepared_entry(
    lsn: Lsn,
    prev_lsn: Lsn,
    database_id: u32,
    prepared: &PreparedWalRecord,
) -> DbResult<Vec<u8>> {
    let requires_framed_format = prev_lsn != Lsn::ZERO
        || prepared.compressed_payload.is_some()
        || database_id != WalEntry::LEGACY_DATABASE_ID;
    if !requires_framed_format {
        return write_entry_with_checksum(lsn, &prepared.payload_bytes);
    }

    let (compression_tag, encoded_payload): (u8, &[u8]) =
        match prepared.compressed_payload.as_deref() {
            Some(payload) if payload.len() < prepared.payload_bytes.len() => {
                (prepared.compression_tag, payload)
            }
            _ => (ENTRY_COMPRESSION_NONE, &prepared.payload_bytes),
        };

    let framed_payload = encode_framed_payload_v3(
        &prepared.payload_bytes,
        compression_tag,
        encoded_payload,
        prev_lsn,
        database_id,
    )?;
    write_entry_with_checksum(lsn, &framed_payload)
}

/// Encode a `WalEntry` into its on-disk binary representation.
///
/// Legacy layout: `payload_len (u32 LE) | lsn (u64 LE) | payload | checksum (u32 LE)`
///
/// When compression is enabled and beneficial, payload bytes are wrapped in a
/// framed v2 envelope before checksumming.
pub fn encode_entry_with_compression(
    entry: &WalEntry,
    compression: WalCompression,
) -> DbResult<Vec<u8>> {
    let prepared = prepare_record_with_compression(&entry.record, compression)?;
    encode_prepared_entry(entry.lsn, entry.prev_lsn, entry.database_id, &prepared)
}

/// Encode a `WalEntry` using the uncompressed payload format.
pub fn encode_entry(entry: &WalEntry) -> DbResult<Vec<u8>> {
    encode_uncompressed_entry(entry)
}

fn decode_unframed_record(lsn: Lsn, payload: &[u8]) -> DbResult<WalEntry> {
    let mut reader = BinaryReader::new(payload);
    let record = read_record_payload(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(DbError::internal("WAL: entry payload has trailing bytes"));
    }
    Ok(WalEntry {
        lsn,
        prev_lsn: Lsn::ZERO,
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record,
    })
}

fn decode_v2_record(lsn: Lsn, payload: &[u8]) -> DbResult<WalEntry> {
    if payload.len() < 7 {
        return Err(DbError::internal("WAL: v2 payload header is truncated"));
    }
    if payload[0] != ENTRY_V2_MARKER {
        return Err(DbError::internal("WAL: invalid v2 payload marker"));
    }

    let format_version = payload[1];
    let (compression_tag, prev_lsn, database_id, original_payload_len, encoded_payload) =
        match format_version {
            ENTRY_FRAMED_FORMAT_VERSION_V1 => {
                let compression_tag = payload[2];
                let original_payload_len = u32_to_usize_checked(
                    u32::from_le_bytes([payload[3], payload[4], payload[5], payload[6]]),
                    "v2 original payload length",
                )?;
                (
                    compression_tag,
                    Lsn::ZERO,
                    WalEntry::LEGACY_DATABASE_ID,
                    original_payload_len,
                    &payload[7..],
                )
            }
            ENTRY_FRAMED_FORMAT_VERSION_V2 => {
                if payload.len() < 15 {
                    return Err(DbError::internal("WAL: v2 payload header is truncated"));
                }
                let compression_tag = payload[2];
                let prev_lsn =
                    Lsn::new(u64::from_le_bytes(payload[3..11].try_into().map_err(
                        |_| DbError::internal("WAL: malformed v2 prev_lsn bytes"),
                    )?));
                let original_payload_len = u32_to_usize_checked(
                    u32::from_le_bytes([payload[11], payload[12], payload[13], payload[14]]),
                    "v2 original payload length",
                )?;
                (
                    compression_tag,
                    prev_lsn,
                    WalEntry::LEGACY_DATABASE_ID,
                    original_payload_len,
                    &payload[15..],
                )
            }
            ENTRY_FRAMED_FORMAT_VERSION_V3 => {
                if payload.len() < 19 {
                    return Err(DbError::internal("WAL: v3 payload header is truncated"));
                }
                let compression_tag = payload[2];
                let prev_lsn =
                    Lsn::new(u64::from_le_bytes(payload[3..11].try_into().map_err(
                        |_| DbError::internal("WAL: malformed v3 prev_lsn bytes"),
                    )?));
                let database_id =
                    u32::from_le_bytes([payload[11], payload[12], payload[13], payload[14]]);
                let original_payload_len = u32_to_usize_checked(
                    u32::from_le_bytes([payload[15], payload[16], payload[17], payload[18]]),
                    "v3 original payload length",
                )?;
                (
                    compression_tag,
                    prev_lsn,
                    database_id,
                    original_payload_len,
                    &payload[19..],
                )
            }
            _ => {
                return Err(DbError::internal(format!(
                    "WAL: unsupported v2 payload format version {format_version}"
                )));
            }
        };

    if original_payload_len > MAX_COMPRESSED_PAYLOAD_BYTES {
        return Err(DbError::internal(format!(
            "WAL: v2 original payload length {original_payload_len} exceeds limit {MAX_COMPRESSED_PAYLOAD_BYTES}"
        )));
    }
    // Reject forged records claiming an absurd decompression ratio so a tiny
    // on-disk record cannot force a 64 MiB heap allocation per replay attempt.
    // The 64 MiB hard cap above already bounds per-record damage; the ratio
    // check below only fires for records that meaningfully amplify (small
    // encoded payload claiming a much larger decoded size).
    if !encoded_payload.is_empty()
        && original_payload_len / encoded_payload.len() > MAX_WAL_DECOMPRESSION_RATIO
        && compression_tag != ENTRY_COMPRESSION_NONE
        && encoded_payload.len() < 4096
    {
        return Err(DbError::internal(format!(
            "WAL: v2 decompression ratio {} -> {} exceeds limit {MAX_WAL_DECOMPRESSION_RATIO}",
            encoded_payload.len(),
            original_payload_len,
        )));
    }

    let decoded_payload = match compression_tag {
        ENTRY_COMPRESSION_NONE => {
            if encoded_payload.len() != original_payload_len {
                return Err(DbError::internal(format!(
                    "WAL: v2 payload length mismatch: expected {original_payload_len}, got {}",
                    encoded_payload.len()
                )));
            }
            encoded_payload.to_vec()
        }
        ENTRY_COMPRESSION_LZ4 => lz4_flex::block::decompress(encoded_payload, original_payload_len)
            .map_err(|error| {
                DbError::internal(format!("WAL: lz4 decompression failed: {error}"))
            })?,
        ENTRY_COMPRESSION_ZSTD => zstd::bulk::decompress(encoded_payload, original_payload_len)
            .map_err(|error| {
                DbError::internal(format!("WAL: zstd decompression failed: {error}"))
            })?,
        _ => {
            return Err(DbError::internal(format!(
                "WAL: unsupported v2 compression tag {compression_tag}"
            )));
        }
    };

    let mut entry = decode_unframed_record(lsn, &decoded_payload)?;
    entry.prev_lsn = prev_lsn;
    entry.database_id = database_id;
    Ok(entry)
}

/// Decode a `WalEntry` from bytes. Returns `(entry, bytes_consumed)`.
///
/// Returns `Err` if data is truncated or checksum fails.
///
/// New entries use CRC32C. For backward compatibility we still accept entries
/// that carry the historical FNV-1a checksum.
pub fn decode_entry(data: &[u8]) -> DbResult<(WalEntry, usize)> {
    if data.len() < ENTRY_HEADER_SIZE {
        return Err(DbError::internal("WAL: not enough data for header"));
    }

    let mut header = BinaryReader::new(data);
    let payload_len = u32_to_usize_checked(header.read_u32()?, "entry payload length")?;

    let total_len = 4 + payload_len;
    if data.len() < total_len {
        return Err(DbError::internal("WAL: truncated entry"));
    }

    // The region after the 4-byte header: lsn (8) + payload + checksum (4).
    let inner = &data[4..total_len];
    if inner.len() < 12 {
        return Err(DbError::internal("WAL: entry too small"));
    }

    // Checksum covers everything except the last 4 bytes (which is the checksum itself).
    let checksum_region = &inner[..inner.len() - 4];
    let mut checksum_bytes = [0u8; 4];
    checksum_bytes.copy_from_slice(&inner[inner.len() - 4..]);
    let stored_checksum = u32::from_le_bytes(checksum_bytes);
    let computed = compute_crc32c(checksum_region);
    if stored_checksum != computed && stored_checksum != compute_legacy_fnv1a(checksum_region) {
        return Err(DbError::internal("WAL: checksum mismatch"));
    }

    if checksum_region.len() < 8 {
        return Err(DbError::internal("WAL: entry too small for LSN"));
    }

    let lsn = Lsn::new(u64::from_le_bytes(
        checksum_region[..8]
            .try_into()
            .map_err(|_| DbError::internal("WAL: malformed LSN bytes"))?,
    ));
    let payload = &checksum_region[8..];
    if payload.is_empty() {
        return Err(DbError::internal("WAL: empty entry payload"));
    }

    let entry = if payload[0] == ENTRY_V2_MARKER {
        decode_v2_record(lsn, payload)?
    } else {
        decode_unframed_record(lsn, payload)?
    };

    Ok((entry, total_len))
}

#[cfg(test)]
mod tests;
#[derive(Clone, Debug)]
pub struct PreparedWalRecord {
    payload_bytes: Vec<u8>,
    compressed_payload: Option<Vec<u8>>,
    compression_tag: u8,
}
