use super::*;
use aiondb_security::scram::{ScramServer, ScramVerifier};
use bytes::Buf;

/// SCRAM messages are short by design (RFC 5802). A legitimate
/// `client-first-message` is well under 1 KiB; we cap a generous 64 KiB to
/// stop an attacker from forcing the server to allocate megabytes of buffer
/// per unauthenticated SASL exchange.
const MAX_SASL_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_SASL_MECHANISM_NAME_BYTES: usize = 128;

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    /// Run a SCRAM-SHA-256 handshake. Returns the SASL initial-response data
    /// (client-first-message) from the first exchange, then drives the full
    /// server-side SCRAM state machine. On success, sends the SASL-final
    /// message; `AuthenticationOk` remains part of the normal startup
    /// response once the engine accepts the credential.
    pub(super) async fn scram_authenticate(
        &mut self,
        verifier: &ScramVerifier,
    ) -> Result<(), DbError> {
        // Step 1: Send AuthenticationSASL with mechanism list.
        let mut w = MessageWriter::new();
        messages::write_auth_sasl(&mut w, &["SCRAM-SHA-256"]);
        w.flush(&mut self.writer).await?;

        // Step 2: Read SASLInitialResponse (tag 'p').
        let raw = self.read_frontend_message_during_startup().await?;
        if raw.tag != b'p' {
            return Err(DbError::protocol(
                "expected SASLInitialResponse from client",
            ));
        }
        if raw.payload.len() > MAX_SASL_PAYLOAD_BYTES {
            return Err(DbError::protocol(
                "SASLInitialResponse exceeds maximum permitted size",
            ));
        }
        let client_first = parse_sasl_initial_response(raw.payload)?;

        // Step 3: Process client-first, send server-first.
        let mut scram = ScramServer::new(verifier.clone())?;
        let server_first = scram.process_client_first(&client_first)?;

        let mut w = MessageWriter::new();
        messages::write_auth_sasl_continue(&mut w, server_first.as_bytes());
        w.flush(&mut self.writer).await?;

        // Step 4: Read SASLResponse (tag 'p').
        let raw = self.read_frontend_message_during_startup().await?;
        if raw.tag != b'p' {
            return Err(DbError::protocol("expected SASLResponse from client"));
        }
        if raw.payload.len() > MAX_SASL_PAYLOAD_BYTES {
            return Err(DbError::protocol(
                "SASLResponse exceeds maximum permitted size",
            ));
        }
        let client_final = String::from_utf8(raw.payload.to_vec())
            .map_err(|_| DbError::protocol("invalid UTF-8 in SASL response"))?;

        // Step 5: Verify and produce server-final.
        let server_final = scram.process_client_final(&client_final)?;

        let mut w = MessageWriter::new();
        messages::write_auth_sasl_final(&mut w, server_final.as_bytes());
        w.flush(&mut self.writer).await?;

        Ok(())
    }
}

/// Parse a `SASLInitialResponse` message payload.
///
/// Format: `mechanism\0` + `length(i32)` + `initial-response-data`
/// Returns the initial response data as a UTF-8 string (client-first-message).
fn parse_sasl_initial_response(mut payload: bytes::BytesMut) -> Result<String, DbError> {
    // Read mechanism name (null-terminated)
    let mechanism = codec::read_cstring_from_buf_with_limit(
        &mut payload,
        MAX_SASL_MECHANISM_NAME_BYTES,
        "SASL mechanism name",
    )?;
    if mechanism != "SCRAM-SHA-256" {
        return Err(DbError::protocol(format!(
            "unsupported SASL mechanism: {mechanism}"
        )));
    }

    // Read length of initial response data
    let data_len = codec::read_i32_from_buf(&mut payload)?;
    if data_len < 0 {
        return Err(DbError::protocol(
            "missing initial response in SASLInitialResponse",
        ));
    }
    let data_len = usize::try_from(data_len)
        .map_err(|_| DbError::protocol("SASLInitialResponse length out of range"))?;
    if payload.remaining() < data_len {
        return Err(DbError::protocol("SASLInitialResponse data truncated"));
    }
    if payload.remaining() > data_len {
        return Err(DbError::protocol(
            "SASLInitialResponse contains unexpected trailing bytes",
        ));
    }

    let data = payload.split_to(data_len);
    String::from_utf8(data.to_vec())
        .map_err(|_| DbError::protocol("invalid UTF-8 in SASL initial response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sasl_initial_response_rejects_oversized_mechanism_name() {
        let mut payload = bytes::BytesMut::new();
        payload.extend_from_slice(&[b'S'; MAX_SASL_MECHANISM_NAME_BYTES + 1]);
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(&0i32.to_be_bytes());

        let error = parse_sasl_initial_response(payload)
            .expect_err("oversized SASL mechanism name must be rejected");
        assert!(error.to_string().contains("SASL mechanism name"));
        assert!(error.to_string().contains("maximum length"));
    }
}
