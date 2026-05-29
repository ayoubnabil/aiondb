//! SCRAM-SHA-256 (RFC 5802 / RFC 7677).
//!
//! Server-side SCRAM state machine with no I/O. The wire protocol layer
//! feeds messages in and reads responses back.

#![allow(clippy::missing_errors_doc)]

use aiondb_core::{DbError, DbResult};
use base64::Engine as _;
use sha2::Sha256;
use std::fmt;
use subtle::ConstantTimeEq;

const DEFAULT_ITERATIONS: u32 = 100_000;
const MIN_ITERATIONS: u32 = 4096;
const MAX_ITERATIONS: u32 = 1_000_000;
const SALT_LENGTH: usize = 16;
const MAX_SALT_LENGTH: usize = 1024;
const MAX_SALT_B64_LENGTH: usize = 2048;
const MAX_KEY_B64_LENGTH: usize = 128;
const MAX_SCRAM_MESSAGE_BYTES: usize = 4096;
const MAX_SCRAM_ATTR_VALUE_BYTES: usize = 2048;
const SCRAM_PASSWORD_HASH_PREFIX: &str = "SCRAM-SHA-256$";

/// SCRAM-SHA-256 verifier derived from a password.
/// The server stores this, never the cleartext password.
#[derive(Clone)]
pub struct ScramVerifier {
    pub iterations: u32,
    pub salt: Vec<u8>,
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

impl fmt::Debug for ScramVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScramVerifier")
            .field("iterations", &self.iterations)
            .field("salt_len", &self.salt.len())
            .field("stored_key", &"**redacted**")
            .field("server_key", &"**redacted**")
            .finish()
    }
}

impl ScramVerifier {
    /// Create from a cleartext password (done once at CREATE ROLE time).
    pub fn from_password(password: &str) -> DbResult<Self> {
        let mut salt = vec![0u8; SALT_LENGTH];
        getrandom::fill(&mut salt)
            .map_err(|e| DbError::internal(format!("failed to generate salt: {e}")))?;
        Self::from_password_with_salt(password, &salt, DEFAULT_ITERATIONS)
    }

    /// Create from a cleartext password with an explicit salt and iteration count.
    pub fn from_password_with_salt(password: &str, salt: &[u8], iterations: u32) -> DbResult<Self> {
        validate_scram_parameters(salt, iterations)?;
        let salted_password = pbkdf2_sha256(password.as_bytes(), salt, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key")?;
        let stored_key = sha256(&client_key);
        let server_key = hmac_sha256(&salted_password, b"Server Key")?;
        Ok(Self {
            iterations,
            salt: salt.to_vec(),
            stored_key,
            server_key,
        })
    }

    #[must_use]
    pub fn to_password_hash_string(&self) -> String {
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(&self.salt);
        let stored_key_b64 = base64::engine::general_purpose::STANDARD.encode(self.stored_key);
        let server_key_b64 = base64::engine::general_purpose::STANDARD.encode(self.server_key);
        format!(
            "{SCRAM_PASSWORD_HASH_PREFIX}{}:{salt_b64}${stored_key_b64}:{server_key_b64}",
            self.iterations
        )
    }

    pub fn from_password_hash_string(value: &str) -> DbResult<Self> {
        let encoded = value
            .strip_prefix(SCRAM_PASSWORD_HASH_PREFIX)
            .ok_or_else(|| DbError::invalid_authorization("invalid SCRAM password hash format"))?;
        let (params, keys) = encoded
            .split_once('$')
            .ok_or_else(|| DbError::invalid_authorization("invalid SCRAM password hash format"))?;
        let (iterations, salt_b64) = params
            .split_once(':')
            .ok_or_else(|| DbError::invalid_authorization("invalid SCRAM password hash format"))?;
        let iterations = iterations.parse::<u32>().map_err(|_| {
            DbError::invalid_authorization("invalid SCRAM password hash iteration count")
        })?;
        if salt_b64.len() > MAX_SALT_B64_LENGTH {
            return Err(DbError::invalid_authorization(
                "invalid SCRAM password hash salt length",
            ));
        }

        let salt = base64::engine::general_purpose::STANDARD
            .decode(salt_b64)
            .map_err(|_| DbError::invalid_authorization("invalid base64 salt in SCRAM hash"))?;
        validate_scram_parameters(&salt, iterations)?;
        let (stored_key_b64, server_key_b64) = keys
            .split_once(':')
            .ok_or_else(|| DbError::invalid_authorization("invalid SCRAM password hash format"))?;
        let stored_key = decode_fixed_key(stored_key_b64, "stored key")?;
        let server_key = decode_fixed_key(server_key_b64, "server key")?;

        Ok(Self {
            iterations,
            salt,
            stored_key,
            server_key,
        })
    }

    #[must_use]
    pub fn verify_password(&self, password: &str) -> bool {
        let Ok(recomputed) = Self::from_password_with_salt(password, &self.salt, self.iterations)
        else {
            return false;
        };
        recomputed.stored_key.ct_eq(&self.stored_key).unwrap_u8() == 1
            && recomputed.server_key.ct_eq(&self.server_key).unwrap_u8() == 1
    }
}

/// Server-side SCRAM state machine. No I/O -- just message processing.
pub struct ScramServer {
    verifier: ScramVerifier,
    server_nonce: String,
    combined_nonce: String,
    client_first_bare: String,
    server_first: String,
    /// Tracks the gs2 prefix the client sent in client-first. Required
    /// to verify the matching base64 channel-binding flag in client-final.
    client_supports_channel_binding: bool,
}

impl ScramServer {
    pub fn new(verifier: ScramVerifier) -> DbResult<Self> {
        let mut nonce_bytes = [0u8; 18];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|e| DbError::internal(format!("failed to generate nonce: {e}")))?;
        let server_nonce = base64::engine::general_purpose::STANDARD.encode(nonce_bytes);
        Ok(Self {
            verifier,
            server_nonce,
            combined_nonce: String::new(),
            client_first_bare: String::new(),
            server_first: String::new(),
            client_supports_channel_binding: false,
        })
    }

    /// Process client-first-message. Returns server-first-message.
    pub fn process_client_first(&mut self, client_first: &str) -> DbResult<String> {
        validate_scram_message_len(client_first, "client-first")?;
        // client-first-message = gs2-header client-first-message-bare
        // RFC 5802 §5.1: accept "n,," (client supports no channel binding)
        // AND "y,," (client supports CB but believes server does not).
        // Reject "p=..." since this implementation does not advertise
        // channel binding support; a client requiring it will see the
        // failure as auth-rejected, matching libpq behaviour.
        let bare = if let Some(rest) = client_first.strip_prefix("n,,") {
            self.client_supports_channel_binding = false;
            rest
        } else if let Some(rest) = client_first.strip_prefix("y,,") {
            self.client_supports_channel_binding = true;
            rest
        } else {
            return Err(DbError::protocol(
                "invalid SCRAM client-first: missing or unsupported gs2 header",
            ));
        };

        // Extract the client nonce from client-first-message-bare
        let client_nonce = scram_attr_value(bare, "r", "client-first")?
            .ok_or_else(|| DbError::protocol("missing client nonce in SCRAM message"))?;

        if client_nonce.is_empty() || client_nonce.len() > 512 {
            return Err(DbError::protocol("invalid SCRAM client nonce length"));
        }

        let combined_nonce = format!("{client_nonce}{}", self.server_nonce);
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(&self.verifier.salt);

        bare.clone_into(&mut self.client_first_bare);
        self.combined_nonce.clone_from(&combined_nonce);
        self.server_first = format!(
            "r={combined_nonce},s={salt_b64},i={}",
            self.verifier.iterations
        );

        Ok(self.server_first.clone())
    }

    /// Process client-final-message. Returns server-final-message on success.
    pub fn process_client_final(&mut self, client_final: &str) -> DbResult<String> {
        validate_scram_message_len(client_final, "client-final")?;
        // Extract proof
        let proof_b64 = scram_attr_value(client_final, "p", "client-final")?
            .ok_or_else(|| DbError::protocol("missing proof in SCRAM client-final"))?;

        let client_proof = base64::engine::general_purpose::STANDARD
            .decode(proof_b64)
            .map_err(|_| DbError::protocol("invalid base64 in SCRAM proof"))?;

        if client_proof.len() != 32 {
            return Err(DbError::protocol("invalid SCRAM proof length"));
        }

        // client-final-message-without-proof is everything before ",p="
        let without_proof = client_final
            .rsplit_once(",p=")
            .map(|(prefix, _)| prefix)
            .ok_or_else(|| DbError::protocol("malformed SCRAM client-final"))?;
        let proof_tail = client_final
            .rsplit_once(",p=")
            .map(|(_, suffix)| suffix)
            .ok_or_else(|| DbError::protocol("malformed SCRAM client-final"))?;
        if proof_tail.contains(',') {
            return Err(DbError::protocol(
                "SCRAM client-final proof must be the final attribute",
            ));
        }

        // RFC 5802: validate the nonce matches the combined nonce from server-first
        let client_final_nonce = scram_attr_value(without_proof, "r", "client-final")?
            .ok_or_else(|| DbError::protocol("missing nonce in SCRAM client-final"))?;
        if client_final_nonce != self.combined_nonce {
            return Err(DbError::invalid_authorization("SCRAM nonce mismatch"));
        }

        // RFC 5802: validate channel binding. The base64-encoded gs2-header
        // depends on the gs2 prefix the client used in client-first:
        //   "n,,"  -> "biws"  (b'n,,')
        //   "y,,"  -> "eSws"  (b'y,,')
        // We accept both; an attacker can't smuggle a different gs2 here
        // because the prefix used in client-first is captured in
        // `client_supports_channel_binding` and must match.
        let channel_binding = scram_attr_value(without_proof, "c", "client-final")?
            .ok_or_else(|| DbError::protocol("missing channel binding in SCRAM client-final"))?;
        let expected_cb = if self.client_supports_channel_binding {
            "eSws"
        } else {
            "biws"
        };
        if channel_binding != expected_cb {
            return Err(DbError::invalid_authorization(
                "SCRAM channel-binding flag does not match gs2 header from client-first",
            ));
        }

        let auth_message = format!(
            "{},{},{without_proof}",
            self.client_first_bare, self.server_first
        );

        let client_signature = hmac_sha256(&self.verifier.stored_key, auth_message.as_bytes())?;

        // Recover ClientKey = ClientProof XOR ClientSignature
        let mut client_key = [0u8; 32];
        for i in 0..32 {
            client_key[i] = client_proof[i] ^ client_signature[i];
        }

        // Verify: SHA-256(ClientKey) == StoredKey
        let computed_stored_key = sha256(&client_key);
        if computed_stored_key
            .ct_eq(&self.verifier.stored_key)
            .unwrap_u8()
            != 1
        {
            return Err(DbError::invalid_authorization(
                "SCRAM authentication failed",
            ));
        }

        // Compute ServerSignature for mutual authentication
        let server_signature = hmac_sha256(&self.verifier.server_key, auth_message.as_bytes())?;
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(server_signature);

        Ok(format!("v={sig_b64}"))
    }
}

// ---------------------------------------------------------------------------
// Client-side SCRAM (used by `aiondb-replication` to authenticate against an
// upstream primary that advertises SCRAM-SHA-256). The server-side and
// client-side state machines share the same crypto helpers below.
// ---------------------------------------------------------------------------

const SCRAM_GS2_HEADER: &str = "n,,";
/// SASL mechanism name as it appears on the wire.
pub const SCRAM_SHA_256_MECHANISM: &str = "SCRAM-SHA-256";

/// Output of [`ScramClient::client_first_message`]. The driver holds it
/// across the round trip so it can compute the proof in
/// [`ScramClient::process_server_first`].
#[derive(Clone, Debug)]
pub struct ScramClientFirst {
    pub message: String,
    pub bare: String,
    pub client_nonce: String,
}

/// Output of [`ScramClient::process_server_first`].
#[derive(Clone)]
pub struct ScramClientFinal {
    pub message: String,
    pub expected_server_signature: [u8; 32],
}

impl fmt::Debug for ScramClientFinal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScramClientFinal")
            .field("message", &self.message)
            .field("expected_server_signature", &"**redacted**")
            .finish()
    }
}

/// Minimal SCRAM-SHA-256 client state machine. Stateless beyond the nonce
/// and first-message bare body it remembers between the two flights.
#[derive(Clone, Debug)]
pub struct ScramClient {
    user: String,
    password: String,
    first: Option<ScramClientFirst>,
    expected_server_signature: Option<[u8; 32]>,
}

impl ScramClient {
    /// Create a fresh client for the given identity. Nonce is generated on
    /// the first call to [`Self::client_first_message`].
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
            first: None,
            expected_server_signature: None,
        }
    }

    /// Build the `client-first-message` (`n,,n=<user>,r=<nonce>`).
    pub fn client_first_message(&mut self) -> DbResult<&ScramClientFirst> {
        let mut nonce = [0u8; 18];
        getrandom::fill(&mut nonce)
            .map_err(|err| DbError::internal(format!("SCRAM nonce generation failed: {err}")))?;
        let client_nonce = base64::engine::general_purpose::STANDARD.encode(nonce);
        let bare = format!(
            "n={user},r={client_nonce}",
            user = scram_escape_username(&self.user)
        );
        let message = format!("{SCRAM_GS2_HEADER}{bare}");
        self.first = Some(ScramClientFirst {
            message,
            bare,
            client_nonce,
        });
        Ok(self
            .first
            .as_ref()
            .expect("just stored ScramClientFirst above"))
    }

    /// Process `server-first-message` and produce `client-final-message`.
    pub fn process_server_first(&mut self, server_first: &str) -> DbResult<ScramClientFinal> {
        let first = self
            .first
            .as_ref()
            .ok_or_else(|| DbError::protocol("SCRAM client_first must run before server_first"))?
            .clone();

        let parsed = parse_scram_server_first(server_first, &first.client_nonce)?;
        validate_scram_parameters(&parsed.salt, parsed.iterations)?;
        let salted_password =
            pbkdf2_sha256(self.password.as_bytes(), &parsed.salt, parsed.iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key")?;
        let stored_key = sha256(&client_key);
        let channel_binding = base64::engine::general_purpose::STANDARD.encode(SCRAM_GS2_HEADER);
        let client_final_no_proof = format!("c={channel_binding},r={}", parsed.combined_nonce);
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(&parsed.salt);
        let auth_message = format!(
            "{bare},r={combined},s={salt_b64},i={iter},{client_final_no_proof}",
            bare = first.bare,
            combined = parsed.combined_nonce,
            iter = parsed.iterations,
        );
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes())?;
        let client_proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let server_key = hmac_sha256(&salted_password, b"Server Key")?;
        let expected_server_signature = hmac_sha256(&server_key, auth_message.as_bytes())?;

        let message = format!(
            "{client_final_no_proof},p={}",
            base64::engine::general_purpose::STANDARD.encode(&client_proof)
        );
        self.expected_server_signature = Some(expected_server_signature);
        Ok(ScramClientFinal {
            message,
            expected_server_signature,
        })
    }

    /// Verify `server-final-message` against the expected `v=` signature.
    pub fn verify_server_final(&self, server_final: &str) -> DbResult<()> {
        let expected = self.expected_server_signature.ok_or_else(|| {
            DbError::protocol("SCRAM server_final received before client_final was produced")
        })?;
        for attr in server_final.split(',') {
            let Some((key, value)) = attr.split_once('=') else {
                continue;
            };
            match key {
                "v" => {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|err| {
                            DbError::protocol(format!(
                                "SCRAM server-final ServerSignature decode failed: {err}"
                            ))
                        })?;
                    if decoded.ct_eq(&expected).into() {
                        return Ok(());
                    }
                    return Err(DbError::invalid_authorization(
                        "SCRAM server signature mismatch",
                    ));
                }
                "e" => {
                    return Err(DbError::invalid_authorization(format!(
                        "SCRAM server reported authentication error: {value}"
                    )));
                }
                _ => {}
            }
        }
        Err(DbError::protocol(
            "SCRAM server-final-message missing ServerSignature (v=)",
        ))
    }
}

struct ScramServerFirstParsed {
    combined_nonce: String,
    salt: Vec<u8>,
    iterations: u32,
}

fn parse_scram_server_first(message: &str, client_nonce: &str) -> DbResult<ScramServerFirstParsed> {
    validate_scram_message_len(message, "server-first")?;
    let combined_nonce = scram_attr_value(message, "r", "server-first")?
        .ok_or_else(|| DbError::protocol("SCRAM server-first-message missing combined nonce"))?
        .to_owned();
    if !combined_nonce.starts_with(client_nonce) {
        return Err(DbError::invalid_authorization(
            "SCRAM server-first nonce does not start with client nonce",
        ));
    }
    let salt_b64 = scram_attr_value(message, "s", "server-first")?
        .ok_or_else(|| DbError::protocol("SCRAM server-first-message missing salt"))?;
    let salt = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .map_err(|err| {
            DbError::protocol(format!(
                "SCRAM server-first salt is not valid base64: {err}"
            ))
        })?;
    let iterations_raw = scram_attr_value(message, "i", "server-first")?
        .ok_or_else(|| DbError::protocol("SCRAM server-first-message missing iterations"))?;
    let iterations = iterations_raw.parse::<u32>().map_err(|err| {
        DbError::protocol(format!(
            "SCRAM server-first iteration count not numeric: {err}"
        ))
    })?;
    Ok(ScramServerFirstParsed {
        combined_nonce,
        salt,
        iterations,
    })
}

fn validate_scram_message_len(message: &str, context: &str) -> DbResult<()> {
    if message.len() > MAX_SCRAM_MESSAGE_BYTES {
        return Err(DbError::protocol(format!(
            "SCRAM {context} message exceeds {MAX_SCRAM_MESSAGE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn scram_attr_value<'a>(message: &'a str, key: &str, context: &str) -> DbResult<Option<&'a str>> {
    let mut found = None;
    for attr in message.split(',') {
        let Some((attr_key, value)) = attr.split_once('=') else {
            return Err(DbError::protocol(format!(
                "SCRAM {context} attribute \"{attr}\" missing '='"
            )));
        };
        if value.len() > MAX_SCRAM_ATTR_VALUE_BYTES {
            return Err(DbError::protocol(format!(
                "SCRAM {context} attribute \"{attr_key}\" exceeds {MAX_SCRAM_ATTR_VALUE_BYTES} bytes"
            )));
        }
        if attr_key == key {
            if found.is_some() {
                return Err(DbError::protocol(format!(
                    "SCRAM {context} contains duplicate \"{key}\" attribute"
                )));
            }
            found = Some(value);
        }
    }
    Ok(found)
}

fn scram_escape_username(user: &str) -> String {
    user.replace('=', "=3D").replace(',', "=2C")
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut output = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut output);
    output
}

use crate::crypto::{hmac_sha256, sha256};

fn validate_scram_parameters(salt: &[u8], iterations: u32) -> DbResult<()> {
    // RFC 7677 sets the iteration floor at 4096. The upper bound prevents a
    // forged catalog row from pinning a login thread in excessive PBKDF2 work.
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
        return Err(DbError::invalid_authorization(
            "invalid SCRAM password hash iteration count",
        ));
    }
    if salt.len() < SALT_LENGTH || salt.len() > MAX_SALT_LENGTH {
        return Err(DbError::invalid_authorization(
            "invalid SCRAM password hash salt length",
        ));
    }
    Ok(())
}

fn decode_fixed_key(value: &str, field: &str) -> DbResult<[u8; 32]> {
    if value.len() > MAX_KEY_B64_LENGTH {
        return Err(DbError::invalid_authorization(format!(
            "invalid SCRAM password hash {field} length"
        )));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            DbError::invalid_authorization(format!("invalid base64 {field} in SCRAM password hash"))
        })?;
    if bytes.len() != 32 {
        return Err(DbError::invalid_authorization(format!(
            "invalid SCRAM password hash {field} length"
        )));
    }
    let mut fixed = [0u8; 32];
    fixed.copy_from_slice(&bytes);
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_first_message_uses_no_channel_binding_header() {
        let mut client = ScramClient::new("alice", "pw");
        let first = client.client_first_message().expect("first").clone();
        assert!(first.message.starts_with("n,,n=alice,r="));
        assert!(first.bare.starts_with("n=alice,r="));
        assert!(!first.client_nonce.is_empty());
    }

    #[test]
    fn full_client_server_roundtrip_authenticates_matching_password() {
        let password = "correcthorsebatterystaple";
        let verifier =
            ScramVerifier::from_password_with_salt(password, b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let mut client = ScramClient::new("alice", password);

        let client_first = client.client_first_message().expect("first").clone();
        let server_first = server
            .process_client_first(&client_first.message)
            .expect("server first");
        let client_final = client
            .process_server_first(&server_first)
            .expect("client final");
        let server_final = server
            .process_client_final(&client_final.message)
            .expect("server final");
        client
            .verify_server_final(&server_final)
            .expect("server signature accepted");
    }

    #[test]
    fn client_rejects_server_signature_for_wrong_password() {
        let verifier =
            ScramVerifier::from_password_with_salt("real", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let mut client = ScramClient::new("alice", "wrong");

        let client_first = client.client_first_message().expect("first").clone();
        let server_first = server
            .process_client_first(&client_first.message)
            .expect("server first");
        let client_final = client
            .process_server_first(&server_first)
            .expect("client final still computes");
        // Server rejects because the proof was computed with the wrong
        // password.
        let err = server
            .process_client_final(&client_final.message)
            .expect_err("server must reject mismatched proof");
        assert!(err.to_string().to_lowercase().contains("scram"));
    }

    #[test]
    fn client_rejects_tampered_server_signature() {
        let mut client = ScramClient::new("alice", "pw");
        let _ = client.client_first_message().expect("first").clone();
        // Skip past the first message by feeding a syntactically valid
        // server-first with the matching nonce prefix so the client
        // pre-computes its expected ServerSignature.
        let nonce = client
            .first
            .as_ref()
            .expect("first stored")
            .client_nonce
            .clone();
        let server_first = format!(
            "r={nonce}server,s={salt},i=4096",
            salt = base64::engine::general_purpose::STANDARD.encode(b"saltsaltsaltsalt"),
        );
        client
            .process_server_first(&server_first)
            .expect("compute client final");
        let err = client
            .verify_server_final("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect_err("server signature must mismatch");
        assert!(err.to_string().to_lowercase().contains("scram"));
    }

    #[test]
    fn verifier_from_password_roundtrip() {
        let v = ScramVerifier::from_password("pencil").unwrap();
        assert_eq!(v.iterations, 100_000);
        assert_eq!(v.salt.len(), 16);
    }

    #[test]
    fn verifier_password_hash_string_roundtrip() {
        let verifier =
            ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
        let encoded = verifier.to_password_hash_string();
        let decoded = ScramVerifier::from_password_hash_string(&encoded).unwrap();
        assert_eq!(decoded.iterations, verifier.iterations);
        assert_eq!(decoded.salt, verifier.salt);
        assert_eq!(decoded.stored_key, verifier.stored_key);
        assert_eq!(decoded.server_key, verifier.server_key);
    }

    #[test]
    fn verifier_verify_password_accepts_matching_secret() {
        let verifier =
            ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
        assert!(verifier.verify_password("pencil"));
        assert!(!verifier.verify_password("wrong"));
    }

    #[test]
    fn client_first_escapes_username_attribute_delimiters() {
        let mut client = ScramClient::new("alice,role=reader", "pw");
        let first = client.client_first_message().expect("first");
        assert!(
            first.message.starts_with("n,,n=alice=2Crole=3Dreader,r="),
            "{}",
            first.message
        );
    }

    #[test]
    fn verifier_rejects_weak_iteration_count() {
        let result = ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4095);
        assert!(result.is_err());
    }

    #[test]
    fn verifier_rejects_oversized_salt_from_hash() {
        let verifier =
            ScramVerifier::from_password_with_salt("pencil", b"salt_for_testing", 4096).unwrap();
        let big_salt = base64::engine::general_purpose::STANDARD.encode(vec![1u8; 1025]);
        let stored_key_b64 = base64::engine::general_purpose::STANDARD.encode(verifier.stored_key);
        let server_key_b64 = base64::engine::general_purpose::STANDARD.encode(verifier.server_key);
        let encoded = format!("SCRAM-SHA-256$4096:{big_salt}${stored_key_b64}:{server_key_b64}");

        assert!(ScramVerifier::from_password_hash_string(&encoded).is_err());
    }

    #[test]
    fn scram_handshake_success() {
        let password = "pencil";
        let salt = b"salt_for_testing";
        let verifier = ScramVerifier::from_password_with_salt(password, salt, 4096).unwrap();

        let mut server = ScramServer::new(verifier).unwrap();

        // Client generates client-first-message
        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let client_first = format!("n,,n=user,r={client_nonce}");

        let server_first = server.process_client_first(&client_first).unwrap();

        // Parse server_first to get combined nonce, salt, iterations
        let combined_nonce = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("r="))
            .unwrap();
        let salt_b64 = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("s="))
            .unwrap();
        let iterations: u32 = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("i="))
            .unwrap()
            .parse()
            .unwrap();

        // Client computes proof
        let salt_bytes = base64::engine::general_purpose::STANDARD
            .decode(salt_b64)
            .unwrap();
        let salted_password = pbkdf2_sha256(password.as_bytes(), &salt_bytes, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key").unwrap();
        let stored_key = sha256(&client_key);

        let client_first_bare = format!("n=user,r={client_nonce}");
        let client_final_without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message =
            format!("{client_first_bare},{server_first},{client_final_without_proof}");

        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes()).unwrap();
        let mut client_proof = [0u8; 32];
        for i in 0..32 {
            client_proof[i] = client_key[i] ^ client_signature[i];
        }
        let proof_b64 = base64::engine::general_purpose::STANDARD.encode(client_proof);

        let client_final = format!("{client_final_without_proof},p={proof_b64}");
        let server_final = server.process_client_final(&client_final).unwrap();
        assert!(server_final.starts_with("v="));
    }

    #[test]
    fn scram_handshake_wrong_password() {
        let verifier =
            ScramVerifier::from_password_with_salt("correct", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();

        let client_first = "n,,n=user,r=clientnonce";
        let server_first = server.process_client_first(client_first).unwrap();

        // Client uses wrong password
        let combined_nonce = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("r="))
            .unwrap();
        let salt_b64 = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("s="))
            .unwrap();
        let iterations: u32 = server_first
            .split(',')
            .find_map(|p| p.strip_prefix("i="))
            .unwrap()
            .parse()
            .unwrap();

        let salt_bytes = base64::engine::general_purpose::STANDARD
            .decode(salt_b64)
            .unwrap();
        let salted_password = pbkdf2_sha256(b"wrong", &salt_bytes, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key").unwrap();
        let stored_key = sha256(&client_key);

        let client_first_bare = "n=user,r=clientnonce";
        let client_final_without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message =
            format!("{client_first_bare},{server_first},{client_final_without_proof}");

        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes()).unwrap();
        let mut client_proof = [0u8; 32];
        for i in 0..32 {
            client_proof[i] = client_key[i] ^ client_signature[i];
        }
        let proof_b64 = base64::engine::general_purpose::STANDARD.encode(client_proof);

        let client_final = format!("{client_final_without_proof},p={proof_b64}");
        let result = server.process_client_final(&client_final);
        assert!(result.is_err());
    }

    #[test]
    fn scram_rejects_empty_nonce() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let result = server.process_client_first("n,,n=user,r=");
        assert!(result.is_err());
    }

    #[test]
    fn scram_rejects_duplicate_client_first_nonce() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let result = server.process_client_first("n,,n=user,r=one,r=two");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn scram_rejects_duplicate_client_final_proof() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        server
            .process_client_first("n,,n=user,r=clientnonce")
            .expect("server first");
        let proof = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let final_message = format!("c=biws,r={},p={proof},p={proof}", server.combined_nonce);

        let result = server.process_client_final(&final_message);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn client_rejects_server_first_iteration_above_limit() {
        let mut client = ScramClient::new("alice", "pw");
        let first = client.client_first_message().expect("first").clone();
        let salt = base64::engine::general_purpose::STANDARD.encode(b"salt_for_testing");
        let server_first = format!(
            "r={}servernonce,s={salt},i={}",
            first.client_nonce,
            MAX_ITERATIONS + 1
        );

        let err = client
            .process_server_first(&server_first)
            .expect_err("excessive server iterations must fail");
        assert!(err.to_string().contains("iteration count"), "{err}");
    }

    #[test]
    fn scram_rejects_oversized_nonce() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let big_nonce = "x".repeat(513);
        let client_first = format!("n,,n=user,r={big_nonce}");
        let result = server.process_client_first(&client_first);
        assert!(result.is_err());
    }

    #[test]
    fn scram_missing_gs2_header() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let result = server.process_client_first("n=user,r=nonce123");
        assert!(result.is_err());
    }

    #[test]
    fn scram_missing_nonce() {
        let verifier =
            ScramVerifier::from_password_with_salt("pw", b"salt_for_testing", 4096).unwrap();
        let mut server = ScramServer::new(verifier).unwrap();
        let result = server.process_client_first("n,,n=user");
        assert!(result.is_err());
    }
}
