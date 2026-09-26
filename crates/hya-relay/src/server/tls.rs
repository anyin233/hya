//! The proxy's own TLS (rustls with the ring provider).

use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use super::{RelayServerError, TlsFiles};

/// Load the PEM files into an acceptor offering ALPN `h2` and `http/1.1`.
pub(crate) fn acceptor(files: &TlsFiles) -> Result<TlsAcceptor, RelayServerError> {
    let fail = |what: &str, error: &dyn std::fmt::Display| {
        RelayServerError::Tls(format!("{what}: {error}"))
    };
    let cert_path = files.cert.display();
    let certs = CertificateDer::pem_file_iter(&files.cert)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|error| fail(&format!("reading certificate {cert_path}"), &error))?;
    if certs.is_empty() {
        return Err(RelayServerError::Tls(format!(
            "no certificate in {cert_path}"
        )));
    }
    let key = PrivateKeyDer::from_pem_file(&files.key).map_err(|error| {
        fail(
            &format!("reading private key {}", files.key.display()),
            &error,
        )
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| fail("protocol versions", &error))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| fail("certificate and key", &error))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}
