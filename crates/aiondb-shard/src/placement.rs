//! Shared shard-placement hashing helpers.
//!
//! Keep row-routing and graph-adjacency routing on the same hashing path so
//! storage and Fabric agree about the shard that owns a value.

use aiondb_core::{DbError, DbResult, Row, Value};
use sha2::{Digest, Sha256};

pub(crate) fn hash_value(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"\x00"),
        Value::Boolean(b) => hasher.update(if *b { b"\x01\x01" } else { b"\x01\x00" }),
        Value::Int(n) => {
            hasher.update(b"\x02");
            hasher.update(n.to_le_bytes());
        }
        Value::BigInt(n) => {
            hasher.update(b"\x03");
            hasher.update(n.to_le_bytes());
        }
        Value::Text(s) => {
            hasher.update(b"\x07");
            hasher.update(s.as_bytes());
        }
        Value::Uuid(bytes) => {
            hasher.update(b"\x11");
            hasher.update(bytes);
        }
        other => {
            hasher.update(b"\xFF");
            hasher.update(format!("{other:?}").as_bytes());
        }
    }
}

pub fn shard_index_for_values<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    shard_count: u32,
) -> DbResult<u32> {
    if shard_count == 0 {
        return Err(DbError::internal("shard_count is 0 in shard config"));
    }

    let mut hasher = Sha256::new();
    let mut hashed_any = false;
    for value in values {
        hashed_any = true;
        hash_value(&mut hasher, value);
    }
    if !hashed_any {
        return Err(DbError::internal("shard key value list is empty"));
    }
    let digest = hasher.finalize();
    let hash = u64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    u32::try_from(hash % u64::from(shard_count))
        .map_err(|_| DbError::internal("hashed shard index exceeds u32"))
}

pub fn shard_index_for_row_values(
    row_values: &[Value],
    shard_key_ordinals: &[usize],
    shard_count: u32,
) -> DbResult<u32> {
    if shard_count == 0 {
        return Err(DbError::internal("shard_count is 0 in shard config"));
    }
    if shard_key_ordinals.is_empty() {
        return Err(DbError::internal("shard key column list is empty"));
    }

    let mut shard_values = Vec::with_capacity(shard_key_ordinals.len());
    for &ordinal in shard_key_ordinals {
        let Some(value) = row_values.get(ordinal) else {
            return Err(DbError::internal(format!(
                "row is missing shard key value at ordinal {ordinal}"
            )));
        };
        shard_values.push(value);
    }
    shard_index_for_values(shard_values, shard_count)
}

pub(crate) fn values_shard_index<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    shard_count: u32,
) -> DbResult<u32> {
    shard_index_for_values(values, shard_count)
}

pub(crate) fn row_shard_index(
    row: &Row,
    shard_key_ordinals: &[usize],
    shard_count: u32,
) -> DbResult<u32> {
    shard_index_for_row_values(&row.values, shard_key_ordinals, shard_count)
}
