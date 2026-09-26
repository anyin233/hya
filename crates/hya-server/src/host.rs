//! Request admission before any route runs (ADR-0025 threat model;
//! docs/protocol/README.md "Allowed Host names").
//!
//! - **Host allowlist** (DNS rebinding): a request that names a host must
//!   name an allowed one — `localhost`, `127.0.0.1`, or `[::1]` on any port,
//!   plus the names the server was started with ([`HostPolicy::with_hosts`]:
//!   `--allow-host`, and a non-wildcard `--bind` host). The port is not
//!   compared: a rebinding page cannot make the browser send a loopback
//!   name, and requests through the relay bridge carry the bridge's own
//!   loopback port. Both the `Host` header and an absolute request URI's
//!   authority (HTTP/2 `:authority`) are checked. A request without either
//!   is refused when it came from a network peer (a TCP listener's
//!   `ConnectInfo`, or the relay); an in-process call (the gRPC binding's
//!   dispatch, tests) names no host and is not a network request.
//! - **No browsers over the relay** ([`Origin::Relay`]): a relay-origin
//!   request carrying `Origin` or a `Sec-Fetch-*` header is refused, which
//!   covers CORS fetches, their preflights, and browser WebSocket handshakes
//!   (browsers always send `Origin` on those).
//!
//! Both refusals answer `403 {"error":{"code":"permission_denied",…}}` (the
//! stable error table; gRPC `PERMISSION_DENIED`).

use std::collections::BTreeSet;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use hya_api::error::Code;

use crate::Origin;
use crate::v1::V1Error;

/// The loopback names every server accepts.
pub const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// The Host names a server accepts (loopback plus configured names).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostPolicy {
    extra: BTreeSet<String>,
}

impl HostPolicy {
    /// Only the loopback names.
    #[must_use]
    pub fn loopback() -> Self {
        Self::default()
    }

    /// Loopback plus `names` (host names or IP addresses, no port; an IPv6
    /// address with or without brackets). Case and a trailing dot are
    /// ignored.
    ///
    /// # Errors
    /// A name that is empty, has a port or a path, or holds characters no
    /// host name has.
    pub fn with_hosts<I, S>(names: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut extra = BTreeSet::new();
        for name in names {
            let normalized = normalize_name(name.as_ref())?;
            if !LOOPBACK_HOSTS.contains(&normalized.as_str()) {
                extra.insert(normalized);
            }
        }
        Ok(Self { extra })
    }

    /// The configured names besides loopback, normalized.
    #[must_use]
    pub fn extra_hosts(&self) -> Vec<String> {
        self.extra.iter().cloned().collect()
    }

    /// Whether `authority` (`host`, `host:port`, `[v6]:port`) names an
    /// allowed host.
    #[must_use]
    pub fn allows(&self, authority: &str) -> bool {
        let Some(host) = authority_host(authority) else {
            return false;
        };
        LOOPBACK_HOSTS.contains(&host.as_str()) || self.extra.contains(&host)
    }

    fn describe(&self) -> String {
        LOOPBACK_HOSTS
            .iter()
            .map(|name| (*name).to_owned())
            .chain(self.extra.iter().cloned())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `name` normalized: lowercase, no trailing dot, IPv6 in brackets.
fn normalize_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let invalid = || {
        format!(
            "--allow-host {trimmed:?}: expected a host name or IP address without a port (for example `hya.example.lan` or `192.168.1.20`)"
        )
    };
    if trimmed.is_empty() {
        return Err(invalid());
    }
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    if let Ok(v6) = bare.parse::<std::net::Ipv6Addr>() {
        return Ok(format!("[{v6}]"));
    }
    let lower = bare.trim_end_matches('.').to_ascii_lowercase();
    let valid = !lower.is_empty()
        && lower.len() <= 253
        && lower.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.' || byte == b'_'
        });
    if valid { Ok(lower) } else { Err(invalid()) }
}

/// The host part of an authority, normalized like [`normalize_name`];
/// `None` when it is malformed (a bad port, stray characters).
fn authority_host(authority: &str) -> Option<String> {
    let authority = authority.trim();
    if authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            other => Some(other.strip_prefix(':')?),
        };
        (format!("[{inside}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (authority.to_owned(), None),
        }
    };
    if let Some(port) = port
        && (port.is_empty() || port.parse::<u16>().is_err())
    {
        return None;
    }
    normalize_name(&host).ok()
}

/// A client-supplied name for an error message: at most 64 printable
/// characters, debug-quoted.
fn shown(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(64)
        .collect();
    format!("{clean:?}")
}

/// Whether `headers` carry a browser's fetch metadata.
pub(crate) fn is_browser(headers: &HeaderMap) -> bool {
    headers.contains_key("origin")
        || headers.contains_key("sec-fetch-site")
        || headers.contains_key("sec-fetch-mode")
        || headers.contains_key("sec-fetch-dest")
}

/// The Host names a request carries: its `Host` header and its URI
/// authority. `Err` for a `Host` header that is not text.
fn named_hosts<'a>(headers: &'a HeaderMap, uri: &'a Uri) -> Result<Vec<&'a str>, ()> {
    let mut names = Vec::new();
    for value in headers.get_all(axum::http::header::HOST) {
        names.push(value.to_str().map_err(|_| ())?);
    }
    if let Some(authority) = uri.authority() {
        names.push(authority.as_str());
    }
    Ok(names)
}

/// Check one request against `policy` (the rules of this module).
pub(crate) fn admit(
    policy: &HostPolicy,
    headers: &HeaderMap,
    uri: &Uri,
    extensions: &axum::http::Extensions,
) -> Result<(), V1Error> {
    let relay = Origin::of(extensions) == Origin::Relay;
    if relay && is_browser(headers) {
        return Err(V1Error::new(
            Code::PermissionDenied,
            "browser requests are not accepted over the relay (a request with Origin or Sec-Fetch-* headers arrived through the relay)",
        ));
    }
    let Ok(names) = named_hosts(headers, uri) else {
        return Err(V1Error::new(
            Code::PermissionDenied,
            "request refused: the Host header is not valid text",
        ));
    };
    if names.is_empty() {
        let network = relay || extensions.get::<ConnectInfo<SocketAddr>>().is_some();
        if network {
            return Err(V1Error::new(
                Code::PermissionDenied,
                "request refused: it names no Host (send a Host header such as 127.0.0.1:<port>)",
            ));
        }
        return Ok(());
    }
    for name in names {
        if !policy.allows(name) {
            return Err(V1Error::new(
                Code::PermissionDenied,
                format!(
                    "request refused: Host {} is not an allowed name for this server (allowed: {}); if you reach this server by that name on purpose, start it with --allow-host <name>",
                    shown(name),
                    policy.describe()
                ),
            ));
        }
    }
    Ok(())
}

/// The axum middleware applying [`admit`] to every request of the router.
pub(crate) async fn guard(policy: Arc<HostPolicy>, request: Request, next: Next) -> Response {
    if let Err(error) = admit(
        &policy,
        request.headers(),
        request.uri(),
        request.extensions(),
    ) {
        return error.into_response();
    }
    next.run(request).await
}

/// A tower layer applying the Host allowlist to a gRPC listener (tonic
/// `Server::layer`): a request whose `:authority` / `Host` is not allowed
/// gets a trailers-only `PERMISSION_DENIED` answer.
#[derive(Clone, Debug)]
pub struct GrpcHostLayer {
    policy: Arc<HostPolicy>,
}

impl GrpcHostLayer {
    /// Guard a gRPC listener with `policy`.
    #[must_use]
    pub fn new(policy: HostPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }
}

impl<S> tower::Layer<S> for GrpcHostLayer {
    type Service = GrpcHostGuard<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcHostGuard {
            inner,
            policy: self.policy.clone(),
        }
    }
}

/// The service of [`GrpcHostLayer`].
#[derive(Clone, Debug)]
pub struct GrpcHostGuard<S> {
    inner: S,
    policy: Arc<HostPolicy>,
}

impl<S, B, RB> tower::Service<axum::http::Request<B>> for GrpcHostGuard<S>
where
    S: tower::Service<axum::http::Request<B>, Response = axum::http::Response<RB>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    RB: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: axum::http::Request<B>) -> Self::Future {
        // The gRPC listener never carries the relay origin; a peer without
        // any Host name is refused like on the HTTP listener.
        let names = named_hosts(request.headers(), request.uri());
        let refusal = match names {
            Err(()) => Some("request refused: the Host header is not valid text".to_owned()),
            Ok(names) if names.is_empty() => {
                Some("request refused: it names no :authority / Host".to_owned())
            }
            Ok(names) => names
                .into_iter()
                .find(|name| !self.policy.allows(name))
                .map(|name| {
                    format!(
                        "request refused: :authority {} is not an allowed name for this server (allowed: {}); start it with --allow-host <name> to accept it",
                        shown(name),
                        self.policy.describe()
                    )
                }),
        };
        if let Some(message) = refusal {
            let mut response = axum::http::Response::new(RB::default());
            let headers = response.headers_mut();
            headers.insert("content-type", HeaderValue::from_static("application/grpc"));
            headers.insert("grpc-status", HeaderValue::from_static("7"));
            if let Ok(value) = HeaderValue::from_str(&percent_encode(&message)) {
                headers.insert("grpc-message", value);
            }
            return Box::pin(async move { Ok(response) });
        }
        Box::pin(self.inner.call(request))
    }
}

/// `grpc-message` percent-encoding (bytes outside printable ASCII and `%`).
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if (0x20..0x7f).contains(&byte) && byte != b'%' {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn loopback_names_on_any_port_are_allowed() {
        let policy = HostPolicy::loopback();
        for ok in [
            "localhost",
            "localhost:3250",
            "LOCALHOST:1",
            "localhost.:80",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]",
            "[::1]:4000",
        ] {
            assert!(policy.allows(ok), "{ok}");
        }
        for refused in [
            "evil.example",
            "evil.example:8080",
            "localhost.evil.example",
            "127.0.0.2:80",
            "0.0.0.0:80",
            "192.168.1.5:8080",
            "[::2]:80",
            "localhost:notaport",
            "localhost:",
            "user@localhost",
            "",
            "[::1",
        ] {
            assert!(!policy.allows(refused), "{refused}");
        }
    }

    #[test]
    fn configured_names_are_normalized_and_allowed() {
        let policy =
            HostPolicy::with_hosts(["Hya.Example.Lan.", "192.168.1.20", "fe80::1", "localhost"])
                .unwrap();
        assert_eq!(
            policy.extra_hosts(),
            vec!["192.168.1.20", "[fe80::1]", "hya.example.lan"]
        );
        assert!(policy.allows("hya.example.lan:8080"));
        assert!(policy.allows("HYA.EXAMPLE.LAN"));
        assert!(policy.allows("192.168.1.20:8080"));
        assert!(policy.allows("[fe80::1]:8080"));
        assert!(!policy.allows("other.example.lan"));
    }

    #[test]
    fn configured_names_with_a_port_or_junk_are_refused() {
        for bad in [
            "",
            "host:8080",
            "http://host",
            "host/path",
            "a b",
            "[::1]:80",
        ] {
            let error = HostPolicy::with_hosts([bad]).unwrap_err();
            assert!(error.contains("--allow-host"), "{bad}: {error}");
        }
    }

    fn parts(
        host: Option<&str>,
        extensions: axum::http::Extensions,
    ) -> (HeaderMap, Uri, axum::http::Extensions) {
        let mut headers = HeaderMap::new();
        if let Some(host) = host {
            headers.insert("host", host.parse().unwrap());
        }
        (headers, Uri::from_static("/v1/health"), extensions)
    }

    #[test]
    fn a_network_request_without_a_host_is_refused_an_in_process_one_is_not() {
        let policy = HostPolicy::loopback();
        let (headers, uri, extensions) = parts(None, axum::http::Extensions::new());
        assert!(admit(&policy, &headers, &uri, &extensions).is_ok());
        let mut network = axum::http::Extensions::new();
        network.insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5000))));
        let (headers, uri, extensions) = parts(None, network);
        assert!(admit(&policy, &headers, &uri, &extensions).is_err());
        let mut relay = axum::http::Extensions::new();
        relay.insert(Origin::Relay);
        let (headers, uri, extensions) = parts(None, relay);
        assert!(admit(&policy, &headers, &uri, &extensions).is_err());
    }

    #[test]
    fn an_absolute_uri_authority_is_checked_too() {
        let policy = HostPolicy::loopback();
        let headers = HeaderMap::new();
        let uri: Uri = "http://evil.example/v1/health".parse().unwrap();
        assert!(admit(&policy, &headers, &uri, &axum::http::Extensions::new()).is_err());
        let uri: Uri = "http://127.0.0.1:9/v1/health".parse().unwrap();
        assert!(admit(&policy, &headers, &uri, &axum::http::Extensions::new()).is_ok());
    }

    #[test]
    fn relay_browser_requests_are_refused_local_ones_are_not() {
        let policy = HostPolicy::loopback();
        for header in [
            "origin",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
        ] {
            let mut relay = axum::http::Extensions::new();
            relay.insert(Origin::Relay);
            let (mut headers, uri, extensions) = parts(Some("127.0.0.1:4000"), relay);
            headers.insert(header, "x".parse().unwrap());
            assert!(
                admit(&policy, &headers, &uri, &extensions).is_err(),
                "{header}"
            );
            let (mut headers, uri, extensions) =
                parts(Some("127.0.0.1:4000"), axum::http::Extensions::new());
            headers.insert(header, "x".parse().unwrap());
            assert!(
                admit(&policy, &headers, &uri, &extensions).is_ok(),
                "{header}"
            );
        }
    }

    #[test]
    fn grpc_messages_are_percent_encoded() {
        assert_eq!(percent_encode("a b%é"), "a b%25%C3%A9");
    }
}
