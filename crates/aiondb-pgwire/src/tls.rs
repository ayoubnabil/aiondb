//! TLS support for the `PostgreSQL` wire protocol server.
//!
//! Handles `SSLRequest` negotiation and TLS stream upgrade using `rustls`.

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use aiondb_core::bounded_io::read_file_capped;
use rustls::pki_types::pem::{Error as PemError, PemObject};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tracing::debug;

use crate::codec::{MAX_MESSAGE_SIZE, SSL_REQUEST};

const MAX_TLS_PEM_FILE_BYTES: u64 = 4 * 1024 * 1024;

fn read_tls_pem_file(path: &str, kind: &str) -> io::Result<Vec<u8>> {
    read_file_capped(path, kind, MAX_TLS_PEM_FILE_BYTES)
}

/// Optional TLS configuration for the pgwire server.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the PEM-encoded server certificate chain.
    pub cert_path: String,
    /// Path to the PEM-encoded server private key.
    pub key_path: String,
    /// Optional path to a PEM-encoded CA certificate for client certificate
    /// verification (mutual TLS). If `None`, client certificates are not
    /// requested.
    pub client_ca_path: Option<String>,
}

/// Build a [`TlsAcceptor`] from PEM certificate and key files.
pub fn build_tls_acceptor(config: &TlsConfig) -> io::Result<TlsAcceptor> {
    let cert_pem = read_tls_pem_file(&config.cert_path, "TLS cert")?;
    let certs = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad cert PEM: {e}")))?;

    let key_pem = read_tls_pem_file(&config.key_path, "TLS key")?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_pem).map_err(|e| {
        if matches!(e, PemError::NoItemsFound) {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no private key found in '{}'", config.key_path),
            )
        } else {
            io::Error::new(io::ErrorKind::InvalidData, format!("bad key PEM: {e}"))
        }
    })?;

    let server_config = if let Some(ca_path) = &config.client_ca_path {
        let ca_pem = read_tls_pem_file(ca_path, "client CA cert")?;
        let ca_certs = rustls::pki_types::CertificateDer::pem_slice_iter(&ca_pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("bad client CA PEM: {e}"),
                )
            })?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in ca_certs {
            roots.add(cert).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid client CA cert: {e}"),
                )
            })?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("client cert verifier: {e}"),
                )
            })?;
        rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("TLS config: {e}")))?
    } else {
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("TLS config: {e}")))?
    };

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Result of TLS negotiation on a new TCP connection.
pub enum NegotiatedStream {
    /// Client did not request TLS, or server declined. The stream is plain TCP.
    /// The `startup_bytes` contain the already-read initial message bytes that
    /// must be prepended to future reads.
    Plain {
        stream: TcpStream,
        startup_bytes: Vec<u8>,
    },
    /// TLS handshake completed successfully.
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

/// Negotiate TLS on a freshly accepted TCP connection.
///
/// Reads the initial startup message. If it is an `SSLRequest`:
/// - With a TLS acceptor: responds 'S' and upgrades the connection.
/// - Without a TLS acceptor: responds 'N' and reads the next startup message.
///
/// If the initial message is NOT an `SSLRequest`, returns it as `startup_bytes`
/// so the connection handler can re-process it.
///
/// When `require_tls` is `true`, plaintext connections are rejected with an
pub async fn negotiate_tls(
    mut stream: TcpStream,
    acceptor: Option<&TlsAcceptor>,
    require_tls: bool,
) -> io::Result<NegotiatedStream> {
    // Read the initial startup message header: 4-byte length + payload.
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let msg_len = usize::try_from(u32::from_be_bytes(len_buf)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "startup message length exceeds platform limits",
        )
    })?;

    if msg_len < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "startup message length too small",
        ));
    }

    let payload_len = msg_len - 4;
    if payload_len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("startup message too large ({payload_len} bytes)"),
        ));
    }
    // Grow the buffer chunk-by-chunk as data arrives, rather than reserving
    // `payload_len` bytes up front. A stalled client that sends only the
    // 4-byte length header (declaring up to MAX_MESSAGE_SIZE) must not pin
    // `payload_len` bytes of allocator capacity per pre-auth connection
    // across the startup_timeout window.
    let mut payload: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while payload.len() < payload_len {
        let want = (payload_len - payload.len()).min(chunk.len());
        let n = stream.read(&mut chunk[..want]).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "startup message truncated",
            ));
        }
        payload.extend_from_slice(&chunk[..n]);
    }

    // Check if this is an SSLRequest (length=8, version=80877103).
    if msg_len == 8 && payload_len == 4 {
        let version_bytes: [u8; 4] = payload[0..4].try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed SSL version bytes")
        })?;
        let version = u32::from_be_bytes(version_bytes);
        if version == SSL_REQUEST {
            if let Some(acc) = acceptor {
                // Accept TLS: send 'S' and upgrade.
                stream.write_all(b"S").await?;
                stream.flush().await?;
                debug!("TLS: sent 'S', starting handshake");
                let tls_stream = acc.accept(stream).await?;
                debug!("TLS: handshake complete");
                return Ok(NegotiatedStream::Tls(Box::new(tls_stream)));
            }
            if require_tls {
                // TLS is required but we have no acceptor - reject.
                stream.write_all(b"N").await?;
                stream.flush().await?;
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "TLS is required but no TLS acceptor is configured",
                ));
            }
            // Decline TLS: send 'N'. Client will retry with normal startup.
            stream.write_all(b"N").await?;
            stream.flush().await?;
            debug!("TLS: declined (no acceptor configured)");

            // Read the next startup message (the real one).
            stream.read_exact(&mut len_buf).await?;
            let msg_len2 = usize::try_from(u32::from_be_bytes(len_buf)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "startup message length exceeds platform limits after SSL decline",
                )
            })?;
            if msg_len2 < 4 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "startup message length too small after SSL decline",
                ));
            }
            let payload_len2 = msg_len2 - 4;
            if payload_len2 > MAX_MESSAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("startup message too large ({payload_len2} bytes)"),
                ));
            }
            // Grow incrementally: same pre-auth memory-pinning concern as the
            // initial startup message read above.
            let mut payload2: Vec<u8> = Vec::new();
            let mut chunk2 = [0u8; 4096];
            while payload2.len() < payload_len2 {
                let want = (payload_len2 - payload2.len()).min(chunk2.len());
                let n = stream.read(&mut chunk2[..want]).await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "startup message truncated after SSL decline",
                    ));
                }
                payload2.extend_from_slice(&chunk2[..n]);
            }

            // Reconstruct the full message bytes for the connection handler.
            let mut startup_bytes = Vec::with_capacity(4 + payload_len2);
            startup_bytes.extend_from_slice(&len_buf);
            startup_bytes.extend_from_slice(&payload2);
            return Ok(NegotiatedStream::Plain {
                stream,
                startup_bytes,
            });
        }
    }

    // Not an SSLRequest - plaintext startup.
    if require_tls {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "TLS is required but client did not request SSL",
        ));
    }
    let mut startup_bytes = Vec::with_capacity(4 + payload_len);
    startup_bytes.extend_from_slice(&len_buf);
    startup_bytes.extend_from_slice(&payload);
    Ok(NegotiatedStream::Plain {
        stream,
        startup_bytes,
    })
}

/// An async reader that serves pre-buffered bytes before delegating to an
/// inner reader. Used to replay startup message bytes that were consumed
/// during TLS negotiation.
pub struct PrependReader<R> {
    buffer: Vec<u8>,
    offset: usize,
    inner: R,
}

impl<R> PrependReader<R> {
    /// Create a new reader that first yields `buffer` bytes, then reads from `inner`.
    pub fn new(buffer: Vec<u8>, inner: R) -> Self {
        Self {
            buffer,
            offset: 0,
            inner,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for PrependReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.offset < this.buffer.len() {
            let remaining = &this.buffer[this.offset..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            this.offset += to_copy;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

/// Check whether TLS certificate and key files exist and are readable.
pub fn validate_tls_config(config: &TlsConfig) -> io::Result<()> {
    if !Path::new(&config.cert_path).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("TLS cert file not found: {}", config.cert_path),
        ));
    }
    if !Path::new(&config.key_path).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("TLS key file not found: {}", config.key_path),
        ));
    }
    if let Some(ca_path) = &config.client_ca_path {
        if !Path::new(ca_path).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("TLS client CA file not found: {ca_path}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tls_test_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aiondb-pgwire-tls-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn validate_tls_config_missing_cert() {
        let config = TlsConfig {
            cert_path: "/nonexistent/cert.pem".to_owned(),
            key_path: "/nonexistent/key.pem".to_owned(),
            client_ca_path: None,
        };
        let err = validate_tls_config(&config).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("cert"));
    }

    #[test]
    fn tls_pem_reader_rejects_oversized_file() {
        let dir = unique_tls_test_dir("oversized-cert");
        let cert = dir.join("cert.pem");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&cert)
            .expect("oversized cert should be creatable");
        file.set_len(MAX_TLS_PEM_FILE_BYTES + 1)
            .expect("oversized cert length should be settable");

        let err = read_tls_pem_file(cert.to_str().unwrap(), "TLS cert").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds maximum"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tls_config_clone() {
        let config = TlsConfig {
            cert_path: "/tmp/cert.pem".to_owned(),
            key_path: "/tmp/key.pem".to_owned(),
            client_ca_path: None,
        };
        let cloned = config.clone();
        assert_eq!(cloned.cert_path, "/tmp/cert.pem");
        assert_eq!(cloned.key_path, "/tmp/key.pem");
        assert!(cloned.client_ca_path.is_none());
    }

    #[tokio::test]
    async fn prepend_reader_serves_buffer_then_inner() {
        use tokio::io::AsyncReadExt;

        let buffer = vec![1, 2, 3, 4];
        let inner: &[u8] = &[5, 6, 7, 8];
        let mut reader = PrependReader::new(buffer, inner);

        let mut out = vec![0u8; 8];
        reader.read_exact(&mut out).await.unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[tokio::test]
    async fn prepend_reader_empty_buffer() {
        use tokio::io::AsyncReadExt;

        let reader = PrependReader::new(vec![], &[10, 20][..]);
        let mut out = vec![0u8; 2];
        tokio::pin!(reader);
        reader.read_exact(&mut out).await.unwrap();
        assert_eq!(out, vec![10, 20]);
    }

    #[test]
    fn tls_config_debug() {
        let config = TlsConfig {
            cert_path: "cert.pem".to_owned(),
            key_path: "key.pem".to_owned(),
            client_ca_path: None,
        };
        let dbg = format!("{config:?}");
        assert!(dbg.contains("cert.pem"));
        assert!(dbg.contains("key.pem"));
    }

    #[test]
    fn validate_tls_config_missing_client_ca() {
        // Create real files for cert and key so validation passes those checks.
        let dir = unique_tls_test_dir("missing-client-ca");
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        std::fs::write(&cert, b"fake").unwrap();
        std::fs::write(&key, b"fake").unwrap();

        let config = TlsConfig {
            cert_path: cert.to_str().unwrap().to_owned(),
            key_path: key.to_str().unwrap().to_owned(),
            client_ca_path: Some("/nonexistent/client_ca.pem".to_owned()),
        };
        let err = validate_tls_config(&config).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("client CA"));

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tls_config_clone_with_client_ca() {
        let config = TlsConfig {
            cert_path: "/tmp/cert.pem".to_owned(),
            key_path: "/tmp/key.pem".to_owned(),
            client_ca_path: Some("/tmp/client_ca.pem".to_owned()),
        };
        let cloned = config.clone();
        assert_eq!(cloned.cert_path, "/tmp/cert.pem");
        assert_eq!(cloned.key_path, "/tmp/key.pem");
        assert_eq!(cloned.client_ca_path.as_deref(), Some("/tmp/client_ca.pem"));
    }
}
