use aiondb_core::{DbError, DbResult, IntervalValue, NumericValue, PgDate, TidValue, Value};
use time::{Date, PrimitiveDateTime, Time};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ValueHashKey {
    Null,
    Int(i32),
    BigInt(i64),
    Real(u32),
    Double(u64),
    Numeric(NumericValue),
    Money(i64),
    Text(String),
    Boolean(bool),
    Blob(Vec<u8>),
    Timestamp(PrimitiveDateTime),
    Date(Date),
    LargeDate(PgDate),
    Time(Time),
    TimeTz(i64, i32),
    Interval(IntervalValue),
    Tid(TidValue),
    PgLsn(u64),
    MacAddr([u8; 6]),
    MacAddr8([u8; 8]),
    Uuid([u8; 16]),
    /// Stores the unix timestamp in nanoseconds for consistent hashing.
    TimestampTz(i128),
    Jsonb(String),
    Array(Vec<ValueHashKey>),
}

pub fn build_hash_key(value: &Value) -> DbResult<ValueHashKey> {
    match value {
        Value::Null => Ok(ValueHashKey::Null),
        Value::Int(value) => Ok(ValueHashKey::Int(*value)),
        Value::BigInt(value) => Ok(ValueHashKey::BigInt(*value)),
        Value::Real(value) => Ok(ValueHashKey::Real(canonical_f32(*value))),
        Value::Double(value) => Ok(ValueHashKey::Double(canonical_f64(*value))),
        Value::Numeric(value) => Ok(ValueHashKey::Numeric(value.clone())),
        Value::Money(value) => Ok(ValueHashKey::Money(*value)),
        Value::Text(value) => Ok(ValueHashKey::Text(value.clone())),
        Value::Boolean(value) => Ok(ValueHashKey::Boolean(*value)),
        Value::Blob(value) => Ok(ValueHashKey::Blob(value.clone())),
        Value::Timestamp(value) => Ok(ValueHashKey::Timestamp(*value)),
        Value::Date(value) => Ok(ValueHashKey::Date(*value)),
        Value::LargeDate(value) => Ok(ValueHashKey::LargeDate(*value)),
        Value::Time(value) => Ok(ValueHashKey::Time(*value)),
        Value::TimeTz(time, offset) => Ok(ValueHashKey::TimeTz(
            timetz_local_micros(*time),
            offset.whole_seconds(),
        )),
        Value::Interval(value) => Ok(ValueHashKey::Interval(value.clone())),
        Value::Tid(value) => Ok(ValueHashKey::Tid(*value)),
        Value::PgLsn(value) => Ok(ValueHashKey::PgLsn(value.raw())),
        Value::MacAddr(value) => Ok(ValueHashKey::MacAddr(*value.as_bytes())),
        Value::MacAddr8(value) => Ok(ValueHashKey::MacAddr8(*value.as_bytes())),
        Value::Uuid(bytes) => Ok(ValueHashKey::Uuid(*bytes)),
        Value::TimestampTz(value) => Ok(ValueHashKey::TimestampTz(value.unix_timestamp_nanos())),
        Value::Jsonb(v) => Ok(ValueHashKey::Jsonb(v.to_string())),
        Value::Vector(_) => Err(DbError::feature_not_supported(
            "VECTOR values cannot be used as hash keys (not supported in GROUP BY, DISTINCT, or JOIN keys)",
        )),
        Value::Array(elements) => {
            let keys: Vec<ValueHashKey> = elements
                .iter()
                .map(build_hash_key)
                .collect::<DbResult<Vec<_>>>()?;
            Ok(ValueHashKey::Array(keys))
        }
    }
}

fn timetz_local_micros(time: Time) -> i64 {
    i64::from(time.hour()) * 3_600_000_000
        + i64::from(time.minute()) * 60_000_000
        + i64::from(time.second()) * 1_000_000
        + i64::from(time.microsecond())
}

fn canonical_f32(value: f32) -> u32 {
    if value.is_nan() {
        f32::NAN.to_bits()
    } else if value == 0.0 {
        0.0f32.to_bits()
    } else {
        value.to_bits()
    }
}

fn canonical_f64(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

#[cfg(test)]
#[path = "hash_key/basic_tests.rs"]
mod basic_tests;

#[cfg(test)]
#[path = "hash_key/edge_tests.rs"]
mod edge_tests;
