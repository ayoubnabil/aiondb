//! TLS bring-up for the replica streaming driver.
//!
//! Implements the PostgreSQL `SSLRequest` preamble and wraps the underlying
//! `TcpStream` with `tokio_rustls` when the negotiated [`SslMode`] requires
//! it. Honours libpq semantics:
//!
//! - `disable`     → no preamble, plaintext always.
//! - `allow`       → preamble; accept plaintext fallback on `N`.
//! - `prefer`      → preamble; accept plaintext fallback on `N`.
//! - `require`     → preamble; reject `N`. Skip cert validation
//!   (a malicious MITM is undetectable by design under libpq's `require`).
//! - `verify-ca`   → like `require`, plus root cert chain validation.
//! - `verify-full` → like `verify-ca`, plus hostname verification.
//!
//! The function returns a `BoxedStream` so the calling code can keep using
//! the same generic `tokio::io::split` plumbing regardless of whether TLS
//! was actually negotiated.

use std::pin::Pin;
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::client::{ConnInfo, SslMode};

/// PG `SSLRequest` magic (`80877103` decimal, `0x04D2_162F`).
const SSL_REQUEST_MAGIC: u32 = 80_877_103;
const MAX_ROOT_CERT_PEM_BYTES: u64 = 4 * 1024 * 1024;

/// Erased async stream backing the rest of the driver after the optional
/// TLS upgrade.
pub(crate) type BoxedStream = Pin<Box<dyn AsyncReadAsyncWrite + Send + Unpin + 'static>>;

/// Trait alias combining the two async I/O traits we need from a stream.
pub(crate) trait AsyncReadAsyncWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + ?Sized> AsyncReadAsyncWrite for T {}

pub(crate) async fn maybe_upgrade_to_tls(
    mut stream: TcpStream,
    conninfo: &ConnInfo,
) -> DbResult<BoxedStream> {
    if !conninfo.sslmode.attempts_tls() {
        return Ok(Box::pin(stream));
    }

    // Send SSLRequest: u32 length=8 (BE) followed by u32 magic (BE).
    let mut preamble = [0u8; 8];
    preamble[0..4].copy_from_slice(&8u32.to_be_bytes());
    preamble[4..8].copy_from_slice(&SSL_REQUEST_MAGIC.to_be_bytes());
    stream
        .write_all(&preamble)
        .await
        .map_err(|err| DbError::internal(format!("SSLRequest write failed: {err}")))?;
    stream
        .flush()
        .await
        .map_err(|err| DbError::internal(format!("SSLRequest flush failed: {err}")))?;

    let mut response = [0u8; 1];
    stream
        .read_exact(&mut response)
        .await
        .map_err(|err| DbError::internal(format!("SSLRequest response read failed: {err}")))?;

    match response[0] {
        b'S' => {
            let connector = build_tls_connector(conninfo)?;
            let server_name = ServerName::try_from(conninfo.host.clone()).map_err(|err| {
                DbError::invalid_input_syntax(
                    "primary_conninfo",
                    &format!(
                        "host \"{}\" is not a valid TLS SNI name: {err}",
                        conninfo.host
                    ),
                )
            })?;
            let tls = connector
                .connect(server_name, stream)
                .await
                .map_err(|err| {
                    DbError::internal(format!("TLS handshake with primary failed: {err}"))
                })?;
            Ok(Box::pin(tls))
        }
        b'N' => {
            if conninfo.sslmode.requires_tls() {
                Err(DbError::internal(format!(
                    "primary refused SSLRequest (got 'N') but sslmode={:?} requires TLS",
                    conninfo.sslmode
                )))
            } else {
                Ok(Box::pin(stream))
            }
        }
        b'E' => Err(DbError::internal(
            "primary returned ErrorResponse to SSLRequest; check the server's ssl configuration",
        )),
        other => Err(DbError::protocol(format!(
            "unexpected SSLRequest response byte {other:#04x}"
        ))),
    }
}

fn build_tls_connector(conninfo: &ConnInfo) -> DbResult<TlsConnector> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = conninfo.ssl_root_cert.as_deref() {
        load_root_cert_pem(path, &mut roots)?;
    }

    let client_config = match conninfo.sslmode {
        SslMode::Disable => unreachable!("sslmode=disable should not call build_tls_connector"),
        SslMode::Allow | SslMode::Prefer | SslMode::Require => {
            // libpq sslmode=require/prefer/allow accept any server cert.
            // The threat model is opportunistic encryption, not auth.
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
                .with_no_client_auth()
        }
        SslMode::VerifyCa => {
            if roots.is_empty() {
                return Err(DbError::invalid_input_syntax(
                    "primary_conninfo",
                    "sslmode=verify-ca requires sslrootcert= to load at least one CA",
                ));
            }
            // verify-ca: PKI chain MUST validate against the configured
            // CA, but the certificate's CN/SAN does NOT need to match the
            // host (libpq semantics). Build a real WebPki verifier for the
            // chain check and wrap it so we always accept whatever name
            // the server presents.
            let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|err| {
                    DbError::internal(format!(
                        "failed to build WebPki verifier for sslmode=verify-ca: {err}"
                    ))
                })?;
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(VerifyCaIgnoresHostname { inner }))
                .with_no_client_auth()
        }
        SslMode::VerifyFull => {
            if roots.is_empty() {
                return Err(DbError::invalid_input_syntax(
                    "primary_conninfo",
                    "sslmode=verify-full requires sslrootcert= to load at least one CA",
                ));
            }
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };

    Ok(TlsConnector::from(Arc::new(client_config)))
}

fn load_root_cert_pem(path: &str, roots: &mut RootCertStore) -> DbResult<()> {
    use std::fs::File;
    use std::io::{Cursor, Read as _};

    let file = File::open(path).map_err(|err| {
        DbError::internal(format!("failed to open sslrootcert \"{path}\": {err}"))
    })?;
    let file_len = file
        .metadata()
        .map_err(|err| {
            DbError::internal(format!(
                "failed to read sslrootcert \"{path}\" metadata: {err}"
            ))
        })?
        .len();
    if file_len > MAX_ROOT_CERT_PEM_BYTES {
        return Err(DbError::internal(format!(
            "sslrootcert \"{path}\" is {file_len} bytes, exceeding maximum {MAX_ROOT_CERT_PEM_BYTES} bytes"
        )));
    }
    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "sslrootcert \"{path}\" size {file_len} does not fit in usize"
        ))
    })?;
    let mut pem = Vec::with_capacity(capacity);
    let mut limited = file.take(MAX_ROOT_CERT_PEM_BYTES.saturating_add(1));
    limited.read_to_end(&mut pem).map_err(|err| {
        DbError::internal(format!("failed to read sslrootcert \"{path}\": {err}"))
    })?;
    if u64::try_from(pem.len()).unwrap_or(u64::MAX) > MAX_ROOT_CERT_PEM_BYTES {
        return Err(DbError::internal(format!(
            "sslrootcert \"{path}\" grew while reading, exceeding maximum {MAX_ROOT_CERT_PEM_BYTES} bytes"
        )));
    }

    let mut reader = Cursor::new(pem.as_slice());
    let mut added = 0usize;
    for cert in rustls_pemfile::certs(&mut reader) {
        let cert = cert.map_err(|err| {
            DbError::internal(format!(
                "invalid certificate in sslrootcert \"{path}\": {err}"
            ))
        })?;
        if let Err(err) = roots.add(cert) {
            return Err(DbError::internal(format!(
                "failed to register CA certificate from \"{path}\": {err}"
            )));
        }
        added += 1;
    }
    if added == 0 {
        return Err(DbError::invalid_input_syntax(
            "primary_conninfo",
            &format!("sslrootcert \"{path}\" did not contain any certificates"),
        ));
    }
    Ok(())
}

/// `sslmode=verify-ca` semantics: the certificate must chain to a trusted
/// root, but the CN/SAN does NOT need to match `host`. We delegate the
/// PKI check to rustls' built-in verifier and override the hostname
/// match by re-running it against a placeholder `ServerName` that the
/// inner verifier accepts unconditionally.
#[derive(Debug)]
struct VerifyCaIgnoresHostname {
    inner: Arc<dyn ServerCertVerifier>,
}

impl ServerCertVerifier for VerifyCaIgnoresHostname {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Probe a series of DNS names that are extremely unlikely to be
        // in any real cert, then -- if the inner verifier rejects them
        // for hostname mismatch -- treat that as success because the
        // chain itself validated for the "real" host name. The trick
        // works because WebPkiServerVerifier reports the chain-level
        // failure with `Error::InvalidCertificate(BadEncoding)` /
        // `OtherError` whereas hostname failure is signalled by
        // `Error::InvalidCertificate(NotValidForName)`.
        let probe = ServerName::try_from("aiondb.replication.invalid").map_err(|err| {
            rustls::Error::General(format!(
                "verify-ca probe ServerName construction failed: {err}"
            ))
        })?;
        match self
            .inner
            .verify_server_cert(end_entity, intermediates, &probe, ocsp_response, now)
        {
            Ok(verified) => Ok(verified),
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            Err(other) => Err(other),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_mode(sslmode: SslMode) -> ConnInfo {
        ConnInfo {
            host: "primary.example".to_owned(),
            port: 5432,
            user: "alice".to_owned(),
            password: None,
            database: None,
            application_name: None,
            slot_name: None,
            sslmode,
            ssl_root_cert: None,
        }
    }

    #[tokio::test]
    async fn disable_skips_preamble_and_returns_plaintext_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // No SSLRequest expected on sslmode=disable.
            let mut buf = [0u8; 8];
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                stream.read_exact(&mut buf),
            )
            .await;
            // Confirm no data arrived in 100ms (i.e. the client did not send
            // SSLRequest).
            assert_eq!(&buf, &[0u8; 8]);
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        let wrapped = maybe_upgrade_to_tls(stream, &conn_with_mode(SslMode::Disable))
            .await
            .unwrap();
        drop(wrapped);
        acceptor.await.unwrap();
    }

    #[tokio::test]
    async fn require_errors_when_server_refuses_tls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut preamble = [0u8; 8];
            stream.read_exact(&mut preamble).await.unwrap();
            stream.write_all(b"N").await.unwrap();
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        match maybe_upgrade_to_tls(stream, &conn_with_mode(SslMode::Require)).await {
            Err(err) => assert!(err.to_string().contains("requires TLS"), "{err}"),
            Ok(_) => panic!("sslmode=require must reject plaintext fallback"),
        }
        acceptor.await.unwrap();
    }

    #[tokio::test]
    async fn prefer_falls_back_to_plaintext_when_server_returns_n() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut preamble = [0u8; 8];
            stream.read_exact(&mut preamble).await.unwrap();
            stream.write_all(b"N").await.unwrap();
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        let wrapped = maybe_upgrade_to_tls(stream, &conn_with_mode(SslMode::Prefer))
            .await
            .expect("sslmode=prefer should accept plaintext fallback");
        drop(wrapped);
        acceptor.await.unwrap();
    }
}
