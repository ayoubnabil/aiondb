//! `uuid-ossp` extension: UUID generation functions.
//!
//! Provides:
//! - `uuid_generate_v1()` - time-based UUID (v1, simplified)
//! - `uuid_generate_v4()` - random UUID (v4)
//! - `uuid_nil()` - nil UUID
//! - `uuid_ns_dns()` / `uuid_ns_url()` / `uuid_ns_oid()` / `uuid_ns_x500()`

use aiondb_core::{DataType, DbError, DbResult, Value};

use crate::{Extension, ExtensionFunction, ExtensionRegistrar};

/// The `uuid-ossp` extension.
pub struct UuidOsspExtension;

impl Extension for UuidOsspExtension {
    fn name(&self) -> &'static str {
        "uuid-ossp"
    }

    fn version(&self) -> &'static str {
        "1.1"
    }

    fn description(&self) -> &'static str {
        "generate universally unique identifiers (UUIDs)"
    }

    fn install(&self, registrar: &mut dyn ExtensionRegistrar) -> DbResult<()> {
        registrar.register_function(ExtensionFunction {
            name: "uuid_generate_v4".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_generate_v4,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_generate_v1".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_generate_v1,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_nil".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_nil,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_ns_dns".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_ns_dns,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_ns_url".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_ns_url,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_ns_oid".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_ns_oid,
        });

        registrar.register_function(ExtensionFunction {
            name: "uuid_ns_x500".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_uuid_ns_x500,
        });

        Ok(())
    }
}

/// Generate a version-4 (random) UUID.
fn eval_uuid_generate_v4(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_generate_v4() takes no arguments"));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| DbError::internal(format!("failed to generate random bytes: {e}")))?;
    // Set version 4 (bits 48..51 = 0100)
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    // Set variant 1 (bits 64..65 = 10)
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Value::Uuid(bytes))
}

/// Generate a version-1 (time-based) UUID.
///
/// This is a simplified implementation that uses random node ID bytes
/// rather than a real MAC address, which is acceptable for most use cases
/// and avoids platform-specific dependencies.
fn eval_uuid_generate_v1(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_generate_v1() takes no arguments"));
    }

    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| DbError::internal(format!("failed to generate random bytes: {e}")))?;

    // Use current time as 100-nanosecond intervals since UUID epoch
    // (October 15, 1582).  We approximate with system time.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let unix_ticks_100ns = now.as_nanos() / 100;
    let unix_ticks_100ns_u64 = u64::try_from(unix_ticks_100ns).unwrap_or(u64::MAX);
    let timestamp = unix_ticks_100ns_u64.saturating_add(UUID_EPOCH_OFFSET);

    // time_low (bytes 0..4)
    let time_low = u32::try_from(timestamp & 0xFFFF_FFFF).unwrap_or(u32::MAX);
    bytes[0..4].copy_from_slice(&time_low.to_be_bytes());
    // time_mid (bytes 4..6)
    let time_mid = ((timestamp >> 32) & 0xFFFF) as u16;
    bytes[4..6].copy_from_slice(&time_mid.to_be_bytes());
    // time_hi_and_version (bytes 6..8) - version 1
    let time_hi = ((timestamp >> 48) & 0x0FFF) as u16;
    bytes[6..8].copy_from_slice(&(time_hi | 0x1000).to_be_bytes());
    // clock_seq_hi_and_variant (byte 8) - variant 1
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    Ok(Value::Uuid(bytes))
}

/// Return the nil UUID (all zeros).
fn eval_uuid_nil(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_nil() takes no arguments"));
    }
    Ok(Value::Uuid([0u8; 16]))
}

/// Return the DNS namespace UUID.
fn eval_uuid_ns_dns(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_ns_dns() takes no arguments"));
    }
    // 6ba7b810-9dad-11d1-80b4-00c04fd430c8
    Ok(Value::Uuid([
        0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]))
}

/// Return the URL namespace UUID.
fn eval_uuid_ns_url(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_ns_url() takes no arguments"));
    }
    // 6ba7b811-9dad-11d1-80b4-00c04fd430c8
    Ok(Value::Uuid([
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]))
}

/// Return the OID namespace UUID.
fn eval_uuid_ns_oid(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_ns_oid() takes no arguments"));
    }
    // 6ba7b812-9dad-11d1-80b4-00c04fd430c8
    Ok(Value::Uuid([
        0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]))
}

/// Return the X.500 namespace UUID.
fn eval_uuid_ns_x500(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("uuid_ns_x500() takes no arguments"));
    }
    // 6ba7b814-9dad-11d1-80b4-00c04fd430c8
    Ok(Value::Uuid([
        0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- uuid_generate_v4() ---

    #[test]
    fn uuid_v4_has_correct_version_and_variant() -> DbResult<()> {
        let result = eval_uuid_generate_v4(&[])?;
        let Value::Uuid(bytes) = result else {
            return Err(DbError::internal("expected Uuid"));
        };
        // Version nibble (upper 4 bits of byte 6) must be 0x4.
        assert_eq!(bytes[6] >> 4, 4, "version nibble must be 4");
        // Variant bits (upper 2 bits of byte 8) must be 10.
        assert_eq!(bytes[8] >> 6, 2, "variant bits must be 10");
        Ok(())
    }

    #[test]
    fn uuid_v4_is_not_all_zeros() -> DbResult<()> {
        let result = eval_uuid_generate_v4(&[])?;
        let Value::Uuid(bytes) = result else {
            return Err(DbError::internal("expected Uuid"));
        };
        assert_ne!(bytes, [0u8; 16], "v4 UUID should not be nil");
        Ok(())
    }

    #[test]
    fn uuid_v4_two_calls_differ() {
        let a = eval_uuid_generate_v4(&[]).unwrap();
        let b = eval_uuid_generate_v4(&[]).unwrap();
        assert_ne!(a, b, "two v4 UUIDs should differ");
    }

    #[test]
    fn uuid_v4_rejects_arguments() {
        let args = vec![Value::Int(1)];
        assert!(eval_uuid_generate_v4(&args).is_err());
    }

    // --- uuid_generate_v1() ---

    #[test]
    fn uuid_v1_has_correct_version_and_variant() -> DbResult<()> {
        let result = eval_uuid_generate_v1(&[])?;
        let Value::Uuid(bytes) = result else {
            return Err(DbError::internal("expected Uuid"));
        };
        // Version nibble (upper 4 bits of byte 6) must be 0x1.
        assert_eq!(bytes[6] >> 4, 1, "version nibble must be 1");
        // Variant bits (upper 2 bits of byte 8) must be 10.
        assert_eq!(bytes[8] >> 6, 2, "variant bits must be 10");
        Ok(())
    }

    #[test]
    fn uuid_v1_rejects_arguments() {
        let args = vec![Value::Int(1)];
        assert!(eval_uuid_generate_v1(&args).is_err());
    }

    // --- uuid_nil() ---

    #[test]
    fn uuid_nil_returns_all_zeros() {
        let result = eval_uuid_nil(&[]).unwrap();
        assert_eq!(result, Value::Uuid([0u8; 16]));
    }

    #[test]
    fn uuid_nil_rejects_arguments() {
        assert!(eval_uuid_nil(&[Value::Int(0)]).is_err());
    }

    // --- namespace UUIDs (RFC 4122 well-known constants) ---

    #[test]
    fn uuid_ns_dns_is_rfc4122() {
        // 6ba7b810-9dad-11d1-80b4-00c04fd430c8
        let result = eval_uuid_ns_dns(&[]).unwrap();
        let expected = Value::Uuid([
            0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ]);
        assert_eq!(result, expected);
    }

    #[test]
    fn uuid_ns_url_is_rfc4122() {
        // 6ba7b811-9dad-11d1-80b4-00c04fd430c8
        let result = eval_uuid_ns_url(&[]).unwrap();
        let expected = Value::Uuid([
            0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ]);
        assert_eq!(result, expected);
    }

    #[test]
    fn uuid_ns_oid_is_rfc4122() {
        // 6ba7b812-9dad-11d1-80b4-00c04fd430c8
        let result = eval_uuid_ns_oid(&[]).unwrap();
        let expected = Value::Uuid([
            0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ]);
        assert_eq!(result, expected);
    }

    #[test]
    fn uuid_ns_x500_is_rfc4122() {
        // 6ba7b814-9dad-11d1-80b4-00c04fd430c8
        let result = eval_uuid_ns_x500(&[]).unwrap();
        let expected = Value::Uuid([
            0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ]);
        assert_eq!(result, expected);
    }

    // --- Extension trait metadata ---

    #[test]
    fn uuid_ossp_extension_metadata() {
        let ext = UuidOsspExtension;
        assert_eq!(ext.name(), "uuid-ossp");
        assert_eq!(ext.version(), "1.1");
        assert!(!ext.description().is_empty());
        assert!(ext.dependencies().is_empty());
    }
}
const UUID_EPOCH_OFFSET: u64 = 0x01B2_1DD2_1381_4000;
