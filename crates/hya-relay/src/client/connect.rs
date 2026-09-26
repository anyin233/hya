//! TCP and TLS toward the first hop.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use super::{ClientError, ProbeFailure, ProbeFailureKind};
use crate::link::RelayAddress;

/// ALPN id of HTTP/2.
pub(crate) const ALPN_H2: &[u8] = b"h2";
/// ALPN id of HTTP/1.1 (the WebSocket binding).
pub(crate) const ALPN_HTTP1: &[u8] = b"http/1.1";

/// A connected byte stream to the first hop.
pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The TLS client settings of one relay client: trust roots plus one
/// rustls config per ALPN.
#[derive(Clone)]
pub(crate) struct Tls {
    h2: Arc<ClientConfig>,
    http1: Arc<ClientConfig>,
}

/// Operating-system roots plus Mozilla's bundle, loaded once per process.
fn base_roots() -> &'static RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Unreadable or malformed system certificates are skipped; the
        // webpki bundle still applies.
        let native = rustls_native_certs::load_native_certs();
        let _ = roots.add_parsable_certificates(native.certs);
        roots
    })
}

impl Tls {
    /// Build the TLS settings, trusting the system and webpki roots plus
    /// every certificate in `extra_ca_pem`.
    pub(crate) fn new(extra_ca_pem: Option<&Path>) -> Result<Self, ClientError> {
        let mut roots = base_roots().clone();
        if let Some(path) = extra_ca_pem {
            let certs = CertificateDer::pem_file_iter(path)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .map_err(|error| {
                    ClientError::Config(format!("reading CA file {}: {error}", path.display()))
                })?;
            if certs.is_empty() {
                return Err(ClientError::Config(format!(
                    "no certificate in CA file {}",
                    path.display()
                )));
            }
            for cert in certs {
                roots.add(cert).map_err(|error| {
                    ClientError::Config(format!("CA file {}: {error}", path.display()))
                })?;
            }
        }
        let config = |alpn: &[&[u8]]| -> Result<Arc<ClientConfig>, ClientError> {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|error| ClientError::Config(format!("TLS setup: {error}")))?
                .with_root_certificates(roots.clone())
                .with_no_client_auth();
            config.alpn_protocols = alpn.iter().map(|id| id.to_vec()).collect();
            Ok(Arc::new(config))
        };
        Ok(Self {
            // Offer HTTP/1.1 too, so an HTTP/1.1-only hop completes the
            // handshake and the missing `h2` is reported as such.
            h2: config(&[ALPN_H2, ALPN_HTTP1])?,
            http1: config(&[ALPN_HTTP1])?,
        })
    }
}

fn failure(kind: ProbeFailureKind, detail: impl Into<String>) -> ProbeFailure {
    ProbeFailure {
        kind,
        detail: detail.into(),
    }
}

/// Open TCP (and TLS when the address is secure).
///
/// With `alpn` = `h2` TLS offers `h2` and `http/1.1` and the server must
/// select `h2`; anything else is a hop that cannot carry gRPC. With
/// `http/1.1` only that is offered (WebSocket upgrades need HTTP/1.1).
pub(crate) async fn connect(
    address: &RelayAddress,
    tls: Option<&Tls>,
    alpn: &'static [u8],
    connect_timeout: Duration,
) -> Result<Box<dyn Io>, ProbeFailure> {
    let host = address.host();
    let bare_host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let target = format!("{host}:{}", address.port());
    let tcp = match timeout(
        connect_timeout,
        TcpStream::connect((bare_host, address.port())),
    )
    .await
    {
        Err(_) => {
            return Err(failure(
                ProbeFailureKind::Connect,
                format!("connecting to {target} timed out after {connect_timeout:?}"),
            ));
        }
        Ok(Err(error)) => {
            return Err(failure(
                ProbeFailureKind::Connect,
                format!("connecting to {target}: {error}"),
            ));
        }
        Ok(Ok(tcp)) => tcp,
    };
    let _ = tcp.set_nodelay(true);
    let Some(tls) = tls.filter(|_| address.is_secure()) else {
        return Ok(Box::new(tcp));
    };
    let config = if alpn == ALPN_H2 {
        tls.h2.clone()
    } else {
        tls.http1.clone()
    };
    let name = ServerName::try_from(bare_host.to_owned()).map_err(|error| {
        failure(
            ProbeFailureKind::Tls,
            format!("{bare_host} is not a valid TLS server name: {error}"),
        )
    })?;
    let stream = match timeout(
        connect_timeout,
        TlsConnector::from(config).connect(name, tcp),
    )
    .await
    {
        Err(_) => {
            return Err(failure(
                ProbeFailureKind::Tls,
                format!("TLS handshake with {target} timed out after {connect_timeout:?}"),
            ));
        }
        Ok(Err(error)) => {
            return Err(failure(
                ProbeFailureKind::Tls,
                format!("TLS handshake with {target}: {error}"),
            ));
        }
        Ok(Ok(stream)) => stream,
    };
    if alpn == ALPN_H2 {
        let negotiated = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
        if negotiated.as_deref() != Some(ALPN_H2) {
            let shown = negotiated.map_or_else(
                || "none".to_owned(),
                |p| String::from_utf8_lossy(&p).into_owned(),
            );
            return Err(failure(
                ProbeFailureKind::NoHttp2,
                format!("{target} did not negotiate HTTP/2 over TLS (ALPN: {shown})"),
            ));
        }
    }
    Ok(Box::new(stream))
}

/// The connector error tonic carries for a failed gRPC connection.
#[derive(Debug)]
pub(crate) struct ConnectFailed(pub(crate) ProbeFailure);

impl std::fmt::Display for ConnectFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ConnectFailed {}
