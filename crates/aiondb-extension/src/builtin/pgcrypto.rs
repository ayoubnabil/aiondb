//! `pgcrypto` extension: cryptographic functions.
//!
//! Provides:
//! - `gen_random_uuid()` - random UUID (v4), identical to the core function
//!   but conventionally loaded via pgcrypto
//! - `gen_random_bytes(count)` - generate `count` random bytes
//! - `digest(data, algorithm)` - compute a hash digest (md5, sha1, sha224,
//!   sha256, sha384, sha512)
//! - `hmac(data, key, algorithm)` - compute HMAC

use aiondb_core::{DataType, DbError, DbResult, Value};
use md5::Md5;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

use crate::{Extension, ExtensionFunction, ExtensionRegistrar};

/// The `pgcrypto` extension.
pub struct PgcryptoExtension;

impl Extension for PgcryptoExtension {
    fn name(&self) -> &'static str {
        "pgcrypto"
    }

    fn version(&self) -> &'static str {
        "1.3"
    }

    fn description(&self) -> &'static str {
        "cryptographic functions"
    }

    fn install(&self, registrar: &mut dyn ExtensionRegistrar) -> DbResult<()> {
        registrar.register_function(ExtensionFunction {
            name: "gen_random_uuid".to_owned(),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
            eval_fn: eval_gen_random_uuid,
        });

        registrar.register_function(ExtensionFunction {
            name: "gen_random_bytes".to_owned(),
            return_type: DataType::Blob,
            min_args: 1,
            max_args: Some(1),
            eval_fn: eval_gen_random_bytes,
        });

        registrar.register_function(ExtensionFunction {
            name: "digest".to_owned(),
            return_type: DataType::Blob,
            min_args: 2,
            max_args: Some(2),
            eval_fn: eval_digest,
        });

        registrar.register_function(ExtensionFunction {
            name: "hmac".to_owned(),
            return_type: DataType::Blob,
            min_args: 3,
            max_args: Some(3),
            eval_fn: eval_hmac,
        });

        Ok(())
    }
}

/// Generate a version-4 (random) UUID.
fn eval_gen_random_uuid(args: &[Value]) -> DbResult<Value> {
    if !args.is_empty() {
        return Err(DbError::internal("gen_random_uuid() takes no arguments"));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| DbError::internal(format!("failed to generate random bytes: {e}")))?;
    // Set version 4
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    // Set variant 1
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Value::Uuid(bytes))
}

/// Generate `count` cryptographically-random bytes.
fn eval_gen_random_bytes(args: &[Value]) -> DbResult<Value> {
    if args.len() != 1 {
        return Err(DbError::internal(
            "gen_random_bytes() requires exactly 1 argument",
        ));
    }
    let count = match &args[0] {
        Value::Int(n) if *n >= 0 => usize::try_from(*n).unwrap_or(usize::MAX),
        Value::BigInt(n) if *n >= 0 => usize::try_from(*n).map_err(|_| {
            DbError::internal("gen_random_bytes() argument exceeds platform limits")
        })?,
        Value::Int(_) | Value::BigInt(_) => {
            return Err(DbError::internal(
                "gen_random_bytes() length must be non-negative",
            ));
        }
        Value::Null => return Ok(Value::Null),
        _ => {
            return Err(DbError::internal(
                "gen_random_bytes() argument must be integer",
            ))
        }
    };
    if count > 1024 {
        return Err(DbError::internal(
            "gen_random_bytes() length must not exceed 1024",
        ));
    }
    let mut buf = vec![0u8; count];
    getrandom::fill(&mut buf)
        .map_err(|e| DbError::internal(format!("failed to generate random bytes: {e}")))?;
    Ok(Value::Blob(buf))
}

/// Compute a hash digest of `data` using the specified `algorithm`.
fn eval_digest(args: &[Value]) -> DbResult<Value> {
    if args.len() != 2 {
        return Err(DbError::internal("digest() requires exactly 2 arguments"));
    }
    if args[0].is_null() || args[1].is_null() {
        return Ok(Value::Null);
    }

    let data = value_to_bytes(&args[0])?;
    let algorithm = match &args[1] {
        Value::Text(s) => s.to_ascii_lowercase(),
        _ => {
            return Err(DbError::internal(
                "digest() algorithm argument must be text",
            ))
        }
    };

    let result = match algorithm.as_str() {
        "md5" => Md5::digest(&data).to_vec(),
        "sha1" => {
            // No sha1 dependency; PG returns 20 bytes - point users to sha256.
            return Err(DbError::internal(
                "digest algorithm \"sha1\" is not supported; use \"sha256\" instead",
            ));
        }
        "sha224" => Sha224::digest(&data).to_vec(),
        "sha256" => Sha256::digest(&data).to_vec(),
        "sha384" => Sha384::digest(&data).to_vec(),
        "sha512" => Sha512::digest(&data).to_vec(),
        other => {
            return Err(DbError::internal(format!(
                "digest(): unknown algorithm \"{other}\""
            )));
        }
    };

    Ok(Value::Blob(result))
}

/// Compute HMAC of `data` with `key` using the specified `algorithm`.
fn eval_hmac(args: &[Value]) -> DbResult<Value> {
    if args.len() != 3 {
        return Err(DbError::internal("hmac() requires exactly 3 arguments"));
    }
    if args[0].is_null() || args[1].is_null() || args[2].is_null() {
        return Ok(Value::Null);
    }

    let data = value_to_bytes(&args[0])?;
    let key = value_to_bytes(&args[1])?;
    let algorithm = match &args[2] {
        Value::Text(s) => s.to_ascii_lowercase(),
        _ => return Err(DbError::internal("hmac() algorithm argument must be text")),
    };

    // HMAC: H((key XOR opad) || H((key XOR ipad) || message))
    // Block sizes: MD5=64, SHA-224/256=64, SHA-384/512=128
    let (block_size, hash_fn): (usize, fn(&[u8]) -> Vec<u8>) = match algorithm.as_str() {
        "md5" => (64, |d| Md5::digest(d).to_vec()),
        "sha224" => (64, |d| Sha224::digest(d).to_vec()),
        "sha256" => (64, |d| Sha256::digest(d).to_vec()),
        "sha384" => (128, |d| Sha384::digest(d).to_vec()),
        "sha512" => (128, |d| Sha512::digest(d).to_vec()),
        other => {
            return Err(DbError::internal(format!(
                "hmac(): unknown algorithm \"{other}\""
            )));
        }
    };

    // Derive the HMAC key (hash if longer than block, then zero-pad).
    let mut hmac_key = if key.len() > block_size {
        hash_fn(&key)
    } else {
        key
    };
    hmac_key.resize(block_size, 0);

    // Inner hash: H((key XOR ipad) || data)
    let mut inner_input: Vec<u8> = hmac_key.iter().map(|b| b ^ 0x36).collect();
    inner_input.extend_from_slice(&data);
    let inner_hash = hash_fn(&inner_input);

    // Outer hash: H((key XOR opad) || inner_hash)
    let mut outer_input: Vec<u8> = hmac_key.iter().map(|b| b ^ 0x5c).collect();
    outer_input.extend_from_slice(&inner_hash);

    Ok(Value::Blob(hash_fn(&outer_input)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn value_to_bytes(v: &Value) -> DbResult<Vec<u8>> {
    match v {
        Value::Text(s) => Ok(s.as_bytes().to_vec()),
        Value::Blob(b) => Ok(b.clone()),
        _ => Err(DbError::internal("expected text or bytea argument")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- digest() tests with well-known cryptographic test vectors ---

    #[test]
    fn digest_sha256_hello() {
        // SHA-256("hello") is a well-known test vector.
        let args = vec![
            Value::Text("hello".to_owned()),
            Value::Text("sha256".to_owned()),
        ];
        let result = eval_digest(&args).unwrap();
        let expected: [u8; 32] = [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ];
        assert_eq!(result, Value::Blob(expected.to_vec()));
    }

    #[test]
    fn digest_sha256_empty() {
        // SHA-256("") is a well-known test vector.
        let args = vec![Value::Text(String::new()), Value::Text("sha256".to_owned())];
        let result = eval_digest(&args).unwrap();
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(result, Value::Blob(expected.to_vec()));
    }

    #[test]
    fn digest_md5_hello() {
        // MD5("hello") = 5d41402abc4b2a76b9719d911017c592
        let args = vec![
            Value::Text("hello".to_owned()),
            Value::Text("md5".to_owned()),
        ];
        let result = eval_digest(&args).unwrap();
        let expected: [u8; 16] = [
            0x5d, 0x41, 0x40, 0x2a, 0xbc, 0x4b, 0x2a, 0x76, 0xb9, 0x71, 0x9d, 0x91, 0x10, 0x17,
            0xc5, 0x92,
        ];
        assert_eq!(result, Value::Blob(expected.to_vec()));
    }

    #[test]
    fn digest_sha512_returns_64_bytes() -> DbResult<()> {
        let args = vec![
            Value::Text("test".to_owned()),
            Value::Text("sha512".to_owned()),
        ];
        let result = eval_digest(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 64);
        Ok(())
    }

    #[test]
    fn digest_sha224_returns_28_bytes() -> DbResult<()> {
        let args = vec![
            Value::Text("test".to_owned()),
            Value::Text("sha224".to_owned()),
        ];
        let result = eval_digest(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 28);
        Ok(())
    }

    #[test]
    fn digest_sha384_returns_48_bytes() -> DbResult<()> {
        let args = vec![
            Value::Text("test".to_owned()),
            Value::Text("sha384".to_owned()),
        ];
        let result = eval_digest(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 48);
        Ok(())
    }

    #[test]
    fn digest_null_input_returns_null() {
        let args = vec![Value::Null, Value::Text("sha256".to_owned())];
        let result = eval_digest(&args).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn digest_null_algorithm_returns_null() {
        let args = vec![Value::Text("data".to_owned()), Value::Null];
        let result = eval_digest(&args).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn digest_unknown_algorithm_errors() {
        let args = vec![
            Value::Text("data".to_owned()),
            Value::Text("blake3".to_owned()),
        ];
        assert!(eval_digest(&args).is_err());
    }

    #[test]
    fn digest_wrong_arg_count_errors() {
        let args = vec![Value::Text("data".to_owned())];
        assert!(eval_digest(&args).is_err());
    }

    #[test]
    fn digest_blob_input() {
        // digest() should accept bytea (Blob) as data input.
        let args = vec![
            Value::Blob(b"hello".to_vec()),
            Value::Text("sha256".to_owned()),
        ];
        let result = eval_digest(&args).unwrap();
        // Should produce the same output as Text("hello").
        let text_args = vec![
            Value::Text("hello".to_owned()),
            Value::Text("sha256".to_owned()),
        ];
        let text_result = eval_digest(&text_args).unwrap();
        assert_eq!(result, text_result);
    }

    // --- hmac() tests ---

    #[test]
    fn hmac_sha256_known_vector() {
        // HMAC-SHA256("", "") is a well-known test vector.
        // Key = "", Data = "" => HMAC-SHA256 =
        // b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad
        let args = vec![
            Value::Text(String::new()),
            Value::Text(String::new()),
            Value::Text("sha256".to_owned()),
        ];
        let result = eval_hmac(&args).unwrap();
        let expected: [u8; 32] = [
            0xb6, 0x13, 0x67, 0x9a, 0x08, 0x14, 0xd9, 0xec, 0x77, 0x2f, 0x95, 0xd7, 0x78, 0xc3,
            0x5f, 0xc5, 0xff, 0x16, 0x97, 0xc4, 0x93, 0x71, 0x56, 0x53, 0xc6, 0xc7, 0x12, 0x14,
            0x42, 0x92, 0xc5, 0xad,
        ];
        assert_eq!(result, Value::Blob(expected.to_vec()));
    }

    #[test]
    fn hmac_md5_returns_16_bytes() -> DbResult<()> {
        let args = vec![
            Value::Text("message".to_owned()),
            Value::Text("key".to_owned()),
            Value::Text("md5".to_owned()),
        ];
        let result = eval_hmac(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 16);
        Ok(())
    }

    #[test]
    fn hmac_null_returns_null() {
        let args = vec![
            Value::Null,
            Value::Text("key".to_owned()),
            Value::Text("sha256".to_owned()),
        ];
        assert_eq!(eval_hmac(&args).unwrap(), Value::Null);
    }

    #[test]
    fn hmac_wrong_arg_count_errors() {
        let args = vec![
            Value::Text("data".to_owned()),
            Value::Text("sha256".to_owned()),
        ];
        assert!(eval_hmac(&args).is_err());
    }

    #[test]
    fn hmac_unknown_algorithm_errors() {
        let args = vec![
            Value::Text("data".to_owned()),
            Value::Text("key".to_owned()),
            Value::Text("blowfish".to_owned()),
        ];
        assert!(eval_hmac(&args).is_err());
    }

    // --- gen_random_bytes() tests ---

    #[test]
    fn gen_random_bytes_correct_length() -> DbResult<()> {
        let args = vec![Value::Int(32)];
        let result = eval_gen_random_bytes(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 32);
        Ok(())
    }

    #[test]
    fn gen_random_bytes_zero_length() {
        let args = vec![Value::Int(0)];
        let result = eval_gen_random_bytes(&args).unwrap();
        assert_eq!(result, Value::Blob(vec![]));
    }

    #[test]
    fn gen_random_bytes_max_1024() -> DbResult<()> {
        let args = vec![Value::Int(1024)];
        let result = eval_gen_random_bytes(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 1024);
        Ok(())
    }

    #[test]
    fn gen_random_bytes_exceeds_1024_errors() {
        let args = vec![Value::Int(1025)];
        assert!(eval_gen_random_bytes(&args).is_err());
    }

    #[test]
    fn gen_random_bytes_bigint_arg() -> DbResult<()> {
        let args = vec![Value::BigInt(16)];
        let result = eval_gen_random_bytes(&args)?;
        let Value::Blob(b) = result else {
            return Err(DbError::internal("expected Blob"));
        };
        assert_eq!(b.len(), 16);
        Ok(())
    }

    #[test]
    fn gen_random_bytes_negative_length_errors() {
        let args = vec![Value::Int(-1)];
        let err = eval_gen_random_bytes(&args).expect_err("negative length must fail");
        assert!(err.report().message.contains("non-negative"));
    }

    #[test]
    fn gen_random_bytes_null_returns_null() {
        let args = vec![Value::Null];
        assert_eq!(eval_gen_random_bytes(&args).unwrap(), Value::Null);
    }

    #[test]
    fn gen_random_bytes_wrong_type_errors() {
        let args = vec![Value::Text("10".to_owned())];
        assert!(eval_gen_random_bytes(&args).is_err());
    }

    #[test]
    fn gen_random_bytes_no_args_errors() {
        let args: Vec<Value> = vec![];
        assert!(eval_gen_random_bytes(&args).is_err());
    }

    // --- gen_random_uuid() via pgcrypto ---

    #[test]
    fn pgcrypto_gen_random_uuid_v4_format() -> DbResult<()> {
        let result = eval_gen_random_uuid(&[])?;
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
    fn pgcrypto_gen_random_uuid_rejects_args() {
        let args = vec![Value::Int(1)];
        assert!(eval_gen_random_uuid(&args).is_err());
    }

    // --- Extension trait metadata ---

    #[test]
    fn pgcrypto_extension_metadata() {
        let ext = PgcryptoExtension;
        assert_eq!(ext.name(), "pgcrypto");
        assert_eq!(ext.version(), "1.3");
        assert!(!ext.description().is_empty());
        assert!(ext.dependencies().is_empty());
    }
}
