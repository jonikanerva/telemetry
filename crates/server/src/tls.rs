//! TLS of the test server (feature `test-server`).
//!
//! The command `serve-test` serves HTTPS, because the Playwright browser
//! `webkit` does not keep a `Secure` cookie with the `__Host-` prefix over
//! `http://localhost` (experiment E4). The login session cookie needs that prefix in all
//! three browser engines of the Playwright tests. The command `serve`
//! serves plain HTTP behind the Railway proxy and has no TLS: the release
//! build does not enable the feature, so it does not compile this module,
//! rustls, or tokio-rustls.
//!
//! - **Versions:** TLS 1.3 and TLS 1.2.
//! - **Crypto provider:** aws-lc-rs, the one rustls provider of the
//!   workspace (STACK.md §7). The configuration names the provider
//!   explicitly, so rustls never looks up a process default provider.
//! - **ALPN:** `http/1.1` only, because the server serves only HTTP/1.1.
//! - **Handshake deadline:** [`HANDSHAKE_TIMEOUT`]. The header read timeout
//!   of hyper starts only after the handshake, so without this deadline a
//!   client that never completes the handshake keeps a connection slot.
//! - **Certificate:** a PEM certificate chain and a PEM private key
//!   (PKCS#8, SEC1, or PKCS#1). `e2e/tls-cert.sh` makes a short-lived
//!   self-signed certificate for `localhost`. No key is in the repository.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// The longest time from the accept of a connection to the end of its
/// TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The TLS acceptor of the test server and its handshake deadline.
#[derive(Clone)]
pub struct TestTls {
    acceptor: TlsAcceptor,
    handshake_timeout: Duration,
}

impl TestTls {
    /// An acceptor for `config` with the deadline `handshake_timeout`.
    #[must_use]
    pub fn new(config: Arc<ServerConfig>, handshake_timeout: Duration) -> Self {
        Self {
            acceptor: TlsAcceptor::from(config),
            handshake_timeout,
        }
    }

    /// Does the TLS handshake on `stream` within the handshake deadline.
    ///
    /// # Errors
    ///
    /// Returns a static cause word: `tls_handshake_failed` or
    /// `tls_handshake_timeout`.
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream<TcpStream>, &'static str> {
        match tokio::time::timeout(self.handshake_timeout, self.acceptor.accept(stream)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(_)) => Err("tls_handshake_failed"),
            Err(_) => Err("tls_handshake_timeout"),
        }
    }
}

impl fmt::Debug for TestTls {
    // The configuration holds the private key. Print no part of it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestTls")
            .field("handshake_timeout", &self.handshake_timeout)
            .finish_non_exhaustive()
    }
}

/// Why a certificate or a key is not usable. Each reason is static text
/// and holds no part of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsFileError {
    /// The certificate file is not valid PEM.
    CertificateNotPem,
    /// The certificate file holds no PEM certificate.
    NoCertificate,
    /// The key file is not valid PEM or holds no PEM private key.
    NoKey,
    /// rustls rejected the key, or the key does not match the certificate.
    KeyRejected,
}

/// The rustls server configuration of the test server.
///
/// # Errors
///
/// Returns [`TlsFileError`] when the PEM text holds no certificate or no
/// key, or when rustls rejects the pair.
pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<ServerConfig, TlsFileError> {
    let chain = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsFileError::CertificateNotPem)?;
    if chain.is_empty() {
        return Err(TlsFileError::NoCertificate);
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| TlsFileError::NoKey)?;
    let mut config = builder()
        .map_err(|_| TlsFileError::KeyRejected)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|_| TlsFileError::KeyRejected)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// The builder with the explicit provider and the protocol versions.
fn builder() -> Result<rustls::ConfigBuilder<ServerConfig, rustls::WantsVerifier>, rustls::Error> {
    ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
}

#[cfg(test)]
pub(crate) mod tests {
    use rustls::server::ResolvesServerCertUsingSni;

    use super::*;

    /// A configuration without a certificate: each handshake fails after
    /// the client hello. The tests of the accept loop use it, because the
    /// repository holds no key and the Rust tests make none.
    /// A PEM section with `label` and the base64 text `body`. The tests
    /// build the armor at run time, so the source holds no private key
    /// block for the secret scanner to report.
    pub(crate) fn pem(label: &str, body: &str) -> Vec<u8> {
        format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----\n").into_bytes()
    }

    pub(crate) fn config_without_certificate() -> Arc<ServerConfig> {
        let config = builder()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(ResolvesServerCertUsingSni::new()));
        Arc::new(config)
    }

    #[test]
    fn the_explicit_provider_supports_tls_1_2_and_1_3() {
        assert!(builder().is_ok());
    }

    #[test]
    fn rejects_files_without_a_certificate_or_a_key() {
        assert_eq!(
            server_config(b"", b"").unwrap_err(),
            TlsFileError::NoCertificate
        );
        assert_eq!(
            server_config(b"not pem at all", b"").unwrap_err(),
            TlsFileError::NoCertificate
        );
        // A section without its end line.
        let mut open = pem("CERTIFICATE", "AAAA");
        open.truncate(open.len() - "-----END CERTIFICATE-----\n".len());
        assert_eq!(
            server_config(&open, &pem("PRIVATE KEY", "AAAA")).unwrap_err(),
            TlsFileError::CertificateNotPem
        );
        let certificate = pem("CERTIFICATE", "AAAA");
        assert_eq!(
            server_config(&certificate, b"").unwrap_err(),
            TlsFileError::NoKey
        );
        assert_eq!(
            server_config(&certificate, &certificate).unwrap_err(),
            TlsFileError::NoKey
        );
    }

    #[test]
    fn rejects_a_key_that_rustls_cannot_use() {
        let certificate = pem("CERTIFICATE", "AAAA");
        let key = pem("PRIVATE KEY", "AAAA");
        assert_eq!(
            server_config(&certificate, &key).unwrap_err(),
            TlsFileError::KeyRejected
        );
    }

    #[test]
    fn the_debug_text_holds_no_configuration() {
        let tls = TestTls::new(config_without_certificate(), HANDSHAKE_TIMEOUT);
        assert_eq!(format!("{tls:?}"), "TestTls { handshake_timeout: 10s, .. }");
    }
}
