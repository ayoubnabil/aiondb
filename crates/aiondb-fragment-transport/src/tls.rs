//! TLS configuration for fragment transport connections.
//!
//! Provides helpers to build rustls connectors (client) and acceptors
//! (server) for encrypted inter-node communication.

use std::io;
use std::sync::Arc;

use aiondb_core::bounded_io::read_file_capped;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const MAX_TLS_PEM_FILE_BYTES: u64 = 4 * 1024 * 1024;

fn read_tls_pem_file(path: &str, kind: &str) -> io::Result<Vec<u8>> {
    read_file_capped(path, kind, MAX_TLS_PEM_FILE_BYTES)
}

/// TLS configuration for the client side of fragment transport.
#[derive(Clone, Debug)]
pub struct TlsClientConfig {
    /// Path to PEM-encoded CA certificate for verifying the server.
    pub ca_cert_path: String,
    /// Optional path to PEM-encoded client certificate (for mutual TLS).
    pub client_cert_path: Option<String>,
    /// Optional path to PEM-encoded client private key (for mutual TLS).
    pub client_key_path: Option<String>,
}

/// TLS configuration for the server side of fragment transport.
#[derive(Clone, Debug)]
pub struct TlsServerConfig {
    /// Path to PEM-encoded server certificate chain.
    pub cert_path: String,
    /// Path to PEM-encoded server private key.
    pub key_path: String,
    /// Optional path to PEM-encoded CA for client certificate verification.
    pub client_ca_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read and parse PEM-encoded certificates from a file.
fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let pem = read_tls_pem_file(path, "cert")?;
    rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad cert PEM in '{path}': {e}"),
            )
        })
}

/// Read and parse a PEM-encoded private key from a file.
fn load_private_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let pem = read_tls_pem_file(path, "key")?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad key PEM in '{path}': {e}"),
            )
        })?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no private key found in '{path}'"),
            )
        })
}

/// Build a [`rustls::RootCertStore`] from PEM-encoded CA certificates.
fn build_root_store(ca_cert_path: &str) -> io::Result<rustls::RootCertStore> {
    let ca_certs = load_certs(ca_cert_path)?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in ca_certs {
        roots.add(cert).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid CA cert in '{ca_cert_path}': {e}"),
            )
        })?;
    }
    Ok(roots)
}

// ---------------------------------------------------------------------------
// Client connector
// ---------------------------------------------------------------------------

/// Build a [`TlsConnector`] for outgoing fragment transport connections.
///
/// Loads the CA certificate for server verification, and optionally loads
/// a client certificate and key for mutual TLS authentication.
///
/// # Errors
///
/// Returns an I/O or TLS configuration error when certificates or keys
/// cannot be loaded or when the client TLS configuration is invalid.
pub fn build_tls_connector(config: &TlsClientConfig) -> io::Result<TlsConnector> {
    let roots = build_root_store(&config.ca_cert_path)?;

    let client_config = match (&config.client_cert_path, &config.client_key_path) {
        (Some(cert_path), Some(key_path)) => {
            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_client_auth_cert(certs, key)
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("client TLS config: {e}"),
                    )
                })?
        }
        _ => rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    };

    Ok(TlsConnector::from(Arc::new(client_config)))
}

// ---------------------------------------------------------------------------
// Server acceptor
// ---------------------------------------------------------------------------

/// Build a [`TlsAcceptor`] for incoming fragment transport connections.
///
/// Loads the server certificate chain and private key, and optionally
/// configures client certificate verification for mutual TLS.
///
/// # Errors
///
/// Returns an I/O or TLS configuration error when certificates or keys
/// cannot be loaded or when the server TLS configuration is invalid.
pub fn build_tls_acceptor(config: &TlsServerConfig) -> io::Result<TlsAcceptor> {
    let certs = load_certs(&config.cert_path)?;
    let key = load_private_key(&config.key_path)?;

    let server_config = if let Some(ca_path) = &config.client_ca_path {
        let roots = build_root_store(ca_path)?;
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

/// V2-09 : production-grade variant of [`build_tls_acceptor`] that
/// refuses to start without a configured `client_ca_path`. The default
/// builder above keeps the lenient behaviour for dev / single-node
/// embedded setups, but production fragment-transport listeners should
/// route through this helper so a forgotten CA path can never silently
/// disable peer authentication.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when `client_ca_path` is
/// `None`. Propagates the same errors as [`build_tls_acceptor`]
/// otherwise.
pub fn build_tls_acceptor_strict(config: &TlsServerConfig) -> io::Result<TlsAcceptor> {
    if config.client_ca_path.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "V2-09 : fragment-transport TLS listener refuses to start without \
             client_ca_path ; an unpinned listener accepts any peer certificate \
             and turns a single leaked AuthToken into RCE-equivalent",
        ));
    }
    build_tls_acceptor(config)
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
            "aiondb-fragment-transport-tls-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn client_config_clone_and_debug() {
        let config = TlsClientConfig {
            ca_cert_path: "/tmp/ca.pem".to_owned(),
            client_cert_path: Some("/tmp/client.pem".to_owned()),
            client_key_path: Some("/tmp/client-key.pem".to_owned()),
        };
        let cloned = config.clone();
        assert_eq!(cloned.ca_cert_path, "/tmp/ca.pem");
        assert_eq!(cloned.client_cert_path.as_deref(), Some("/tmp/client.pem"));
        assert_eq!(
            cloned.client_key_path.as_deref(),
            Some("/tmp/client-key.pem")
        );
        let dbg = format!("{config:?}");
        assert!(dbg.contains("ca.pem"));
    }

    #[test]
    fn client_config_without_mutual_tls() {
        let config = TlsClientConfig {
            ca_cert_path: "/tmp/ca.pem".to_owned(),
            client_cert_path: None,
            client_key_path: None,
        };
        let cloned = config.clone();
        assert!(cloned.client_cert_path.is_none());
        assert!(cloned.client_key_path.is_none());
    }

    #[test]
    fn server_config_clone_and_debug() {
        let config = TlsServerConfig {
            cert_path: "/tmp/server.pem".to_owned(),
            key_path: "/tmp/server-key.pem".to_owned(),
            client_ca_path: Some("/tmp/client-ca.pem".to_owned()),
        };
        let cloned = config.clone();
        assert_eq!(cloned.cert_path, "/tmp/server.pem");
        assert_eq!(cloned.key_path, "/tmp/server-key.pem");
        assert_eq!(cloned.client_ca_path.as_deref(), Some("/tmp/client-ca.pem"));
        let dbg = format!("{config:?}");
        assert!(dbg.contains("server.pem"));
    }

    #[test]
    fn server_config_without_mutual_tls() {
        let config = TlsServerConfig {
            cert_path: "/tmp/server.pem".to_owned(),
            key_path: "/tmp/server-key.pem".to_owned(),
            client_ca_path: None,
        };
        let cloned = config.clone();
        assert!(cloned.client_ca_path.is_none());
    }

    #[test]
    fn build_connector_fails_on_missing_ca() {
        let config = TlsClientConfig {
            ca_cert_path: "/nonexistent/ca.pem".to_owned(),
            client_cert_path: None,
            client_key_path: None,
        };
        let result = build_tls_connector(&config);
        let err = result.err().expect("expected error for missing CA");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("ca.pem"));
    }

    #[test]
    fn build_acceptor_fails_on_missing_cert() {
        let config = TlsServerConfig {
            cert_path: "/nonexistent/server.pem".to_owned(),
            key_path: "/nonexistent/key.pem".to_owned(),
            client_ca_path: None,
        };
        let result = build_tls_acceptor(&config);
        let err = result.err().expect("expected error for missing cert");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("server.pem"));
    }

    #[test]
    fn build_acceptor_fails_on_missing_key() {
        // Create a valid cert file but missing key file.
        let dir = unique_tls_test_dir("missing-key");
        let cert = dir.join("cert.pem");
        // Write a minimal (invalid but parseable for file-read purposes) cert.
        std::fs::write(&cert, b"not a real cert").unwrap();

        let config = TlsServerConfig {
            cert_path: cert.to_str().unwrap().to_owned(),
            key_path: "/nonexistent/key.pem".to_owned(),
            client_ca_path: None,
        };
        // Should fail on either cert parse or key read - just verify it errors.
        assert!(build_tls_acceptor(&config).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_certs_reports_path_in_error() {
        let err = load_certs("/does/not/exist/cert.pem").unwrap_err();
        assert!(err.to_string().contains("/does/not/exist/cert.pem"));
    }

    #[test]
    fn load_certs_rejects_oversized_pem_file() {
        let dir = unique_tls_test_dir("oversized-cert");
        let cert_path = dir.join("oversized.pem");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&cert_path)
            .expect("oversized cert file should be creatable");
        file.set_len(MAX_TLS_PEM_FILE_BYTES + 1)
            .expect("oversized cert length should be settable");

        let err = load_certs(cert_path.to_str().unwrap()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds maximum"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_private_key_reports_path_in_error() {
        let err = load_private_key("/does/not/exist/key.pem").unwrap_err();
        assert!(err.to_string().contains("/does/not/exist/key.pem"));
    }

    #[test]
    fn build_root_store_reports_path_in_error() {
        let err = build_root_store("/does/not/exist/ca.pem").unwrap_err();
        assert!(err.to_string().contains("/does/not/exist/ca.pem"));
    }

    #[test]
    fn v2_09_strict_acceptor_rejects_missing_client_ca() {
        let config = TlsServerConfig {
            cert_path: "/whatever/server.pem".to_owned(),
            key_path: "/whatever/server-key.pem".to_owned(),
            client_ca_path: None,
        };
        let err = match build_tls_acceptor_strict(&config) {
            Ok(_) => panic!("strict acceptor must refuse missing client_ca_path"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("V2-09"));
    }

    #[test]
    fn load_private_key_empty_file_errors() {
        let dir = unique_tls_test_dir("empty-key");
        let key_path = dir.join("empty.pem");
        std::fs::write(&key_path, b"").unwrap();

        let err = load_private_key(key_path.to_str().unwrap()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("no private key"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
