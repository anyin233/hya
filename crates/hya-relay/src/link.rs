//! The relay link: the one string a client needs to reach a backend through
//! a relay, and the credential that grants control of it.
//!
//! Grammar:
//!
//! ```text
//! hya://<host>[:port][/<prefix>]/<room_id>[?t=auto|grpc|ws]#<b64url(x25519_pub)>.<b64url(psk)>
//! hya+insecure://…   (same, plaintext toward the first hop)
//! ```
//!
//! - `hya://` means TLS toward the first hop, default port 443;
//!   `hya+insecure://` means plaintext (LAN, tailnet, dev), default port 80.
//! - `<prefix>` is an optional path prefix (one or more segments) under which
//!   the relay is published; the last path segment is the room id.
//! - `t` is the transport binding hint; absent means `auto`. Other query
//!   parameters are ignored for forward compatibility.
//! - The fragment carries the backend's Noise static public key (X25519) and
//!   the pre-shared key, each 32 bytes, unpadded base64url. Fragments are
//!   never sent over the network by URL-handling software, and this type
//!   never prints them except through [`RelayLink::to_secret_string`].
//!
//! Formatting is canonical: the host is lowercased, a default port and
//! `t=auto` are omitted.

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Length of a room id in characters.
pub const ROOM_ID_LEN: usize = 26;

/// Default port of `hya://` links (TLS).
pub const DEFAULT_SECURE_PORT: u16 = 443;

/// Default port of `hya+insecure://` links (plaintext).
pub const DEFAULT_INSECURE_PORT: u16 = 80;

const SECURE_SCHEME: &str = "hya";
const INSECURE_SCHEME: &str = "hya+insecure";
const BASE32_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Which 32-byte key in the link fragment an error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyField {
    /// The backend's Noise static public key (X25519).
    ServerKey,
    /// The pre-shared key.
    Psk,
}

impl fmt::Display for KeyField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyField::ServerKey => "server key",
            KeyField::Psk => "psk",
        })
    }
}

/// A relay link or proxy URL failed validation. Messages never contain key
/// material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The scheme is not `hya`/`hya+insecure` (links) or `http`/`https`
    /// (proxy URLs).
    #[error("unknown relay scheme {0:?}")]
    UnknownScheme(String),
    /// The host is empty or contains characters outside a DNS name, IPv4
    /// address, or bracketed IPv6 address (userinfo is not allowed).
    #[error("invalid relay host {0:?}")]
    InvalidHost(String),
    /// The port is not a number in `1..=65535`.
    #[error("invalid relay port {0:?}")]
    InvalidPort(String),
    /// A path segment is empty, `.`/`..`, or has reserved characters.
    #[error("invalid relay path {0:?}")]
    InvalidPath(String),
    /// The link has no room id path segment.
    #[error("relay link has no room id")]
    MissingRoom,
    /// The room id is not 26 lowercase base32 characters.
    #[error("invalid relay room id {0:?}")]
    InvalidRoomId(String),
    /// The `t` parameter is not `auto`, `grpc`, or `ws`.
    #[error("unknown relay transport {0:?} (expected auto, grpc, or ws)")]
    UnknownTransport(String),
    /// The query string is ambiguous (for example `t` given twice).
    #[error("invalid relay link query: {0}")]
    InvalidQuery(String),
    /// The link has no `#<key>.<psk>` fragment.
    #[error("relay link has no key fragment")]
    MissingFragment,
    /// The fragment is not exactly two `.`-separated parts.
    #[error("relay link key fragment must be <server key>.<psk>")]
    InvalidFragment,
    /// A fragment part is not unpadded base64url.
    #[error("relay link {field} is not unpadded base64url")]
    InvalidBase64 {
        /// Which key.
        field: KeyField,
    },
    /// A fragment part does not decode to 32 bytes.
    #[error("relay link {field} must be 32 bytes, got {len}")]
    InvalidKeyLength {
        /// Which key.
        field: KeyField,
        /// Decoded length.
        len: usize,
    },
}

/// A relay room id: the first 26 characters of the lowercase, unpadded
/// base32 encoding of `sha256(ed25519_pub)` (130 bits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomId(String);

impl RoomId {
    /// Derive the room id owned by an Ed25519 public key.
    #[must_use]
    pub fn from_ed25519(pubkey: &[u8; 32]) -> Self {
        let digest = Sha256::digest(pubkey);
        let mut encoded = data_encoding::BASE32_NOPAD.encode(&digest);
        encoded.make_ascii_lowercase();
        encoded.truncate(ROOM_ID_LEN);
        RoomId(encoded)
    }

    /// Validate a room id string.
    ///
    /// # Errors
    /// [`LinkError::InvalidRoomId`] unless `text` is exactly 26 characters
    /// of the lowercase base32 alphabet (`a-z`, `2-7`).
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        if text.len() == ROOM_ID_LEN && text.bytes().all(|b| BASE32_ALPHABET.contains(&b)) {
            Ok(RoomId(text.to_owned()))
        } else {
            Err(LinkError::InvalidRoomId(text.to_owned()))
        }
    }

    /// The room id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RoomId {
    type Err = LinkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        RoomId::parse(text)
    }
}

/// Derive the room id owned by an Ed25519 public key
/// (same as [`RoomId::from_ed25519`]).
#[must_use]
pub fn room_id_from_ed25519(pubkey: &[u8; 32]) -> RoomId {
    RoomId::from_ed25519(pubkey)
}

/// The transport binding hint (`t=` in the link).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Transport {
    /// Try gRPC, fall back to WebSocket when an intermediary breaks it.
    #[default]
    Auto,
    /// gRPC (HTTP/2) only.
    Grpc,
    /// WebSocket only.
    Ws,
}

impl Transport {
    /// The `t=` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Auto => "auto",
            Transport::Grpc => "grpc",
            Transport::Ws => "ws",
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Transport {
    type Err = LinkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "auto" => Ok(Transport::Auto),
            "grpc" => Ok(Transport::Grpc),
            "ws" => Ok(Transport::Ws),
            other => Err(LinkError::UnknownTransport(other.to_owned())),
        }
    }
}

/// A WebSocket binding route under `<prefix>/hya.relay.v1/ws/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsRoute {
    /// Host control stream.
    Host,
    /// Host side of a data stream.
    Accept,
    /// Client side of a data stream.
    Open,
}

impl WsRoute {
    /// The final path segment.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WsRoute::Host => "host",
            WsRoute::Accept => "accept",
            WsRoute::Open => "open",
        }
    }
}

/// Where a relay is published: TLS or plaintext, host, port, path prefix.
///
/// This is the public address given to `hya serve --relay`, never the
/// proxy's own listen address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelayAddress {
    secure: bool,
    host: String,
    port: u16,
    prefix: String,
}

impl RelayAddress {
    /// Build an address. `host` is a DNS name, IPv4 address, or bracketed
    /// IPv6 address; `port` defaults to 443 (secure) or 80 (plaintext);
    /// `prefix` is `""` or one or more path segments (`"/a/b"`, `"a/b"`, a
    /// trailing `/` is ignored).
    ///
    /// # Errors
    /// [`LinkError::InvalidHost`], [`LinkError::InvalidPort`] (port 0), or
    /// [`LinkError::InvalidPath`].
    pub fn new(
        secure: bool,
        host: &str,
        port: Option<u16>,
        prefix: &str,
    ) -> Result<Self, LinkError> {
        let host = parse_host(host)?;
        let port = match port {
            Some(0) => return Err(LinkError::InvalidPort("0".to_owned())),
            Some(port) => port,
            None => default_port(secure),
        };
        let prefix = parse_prefix(prefix.strip_suffix('/').unwrap_or(prefix))?;
        Ok(RelayAddress {
            secure,
            host,
            port,
            prefix,
        })
    }

    /// Parse a proxy URL such as `https://relay.example.com/hya` or
    /// `http://100.64.0.7:8766`.
    ///
    /// # Errors
    /// [`LinkError::UnknownScheme`] for anything but `http`/`https`,
    /// [`LinkError::InvalidPath`] for a query or fragment, plus the
    /// [`RelayAddress::new`] errors.
    pub fn parse_proxy_url(url: &str) -> Result<Self, LinkError> {
        let (scheme, rest) = split_scheme(url)?;
        let secure = match scheme {
            "https" => true,
            "http" => false,
            other => return Err(LinkError::UnknownScheme(other.to_owned())),
        };
        if rest.contains(['?', '#']) {
            return Err(LinkError::InvalidPath(rest.to_owned()));
        }
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let (host, port) = split_authority(authority)?;
        let path = path.strip_suffix('/').unwrap_or(path);
        Ok(RelayAddress {
            secure,
            host: parse_host(host)?,
            port: port.unwrap_or(default_port(secure)),
            prefix: parse_prefix(path)?,
        })
    }

    /// Whether the first hop uses TLS (`hya://`, `https`).
    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// The lowercased host; IPv6 addresses keep their brackets.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The effective port (explicit or default).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The path prefix: `""` or `/seg[/seg…]` without a trailing slash.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `host[:port]`, omitting the default port.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.port == default_port(self.secure) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// `https://host[:port]` or `http://host[:port]`.
    #[must_use]
    pub fn origin(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        format!("{scheme}://{}", self.authority())
    }

    /// The origin plus the path prefix.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("{}{}", self.origin(), self.prefix)
    }

    /// Base URL of the gRPC binding; rpc paths
    /// (`/hya.relay.v1.Relay/<Method>`) are appended below the prefix.
    #[must_use]
    pub fn grpc_url(&self) -> String {
        self.base_url()
    }

    /// Full URL of a WebSocket binding route:
    /// `ws[s]://host[:port]<prefix>/hya.relay.v1/ws/<route>`.
    #[must_use]
    pub fn ws_url(&self, route: WsRoute) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        format!(
            "{scheme}://{}{}/hya.relay.v1/ws/{}",
            self.authority(),
            self.prefix,
            route.as_str()
        )
    }

    fn link_scheme(&self) -> &'static str {
        if self.secure {
            SECURE_SCHEME
        } else {
            INSECURE_SCHEME
        }
    }
}

/// A parsed relay link: relay address, room, transport hint, and the two
/// secret keys.
///
/// [`Display`](fmt::Display) and [`Debug`] never print the keys; use
/// [`RelayLink::to_secret_string`] to produce the shareable link.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayLink {
    address: RelayAddress,
    room_id: RoomId,
    transport: Transport,
    server_key: [u8; 32],
    psk: [u8; 32],
}

impl RelayLink {
    /// Assemble a link from its parts.
    #[must_use]
    pub fn new(
        address: RelayAddress,
        room_id: RoomId,
        transport: Transport,
        server_key: [u8; 32],
        psk: [u8; 32],
    ) -> Self {
        RelayLink {
            address,
            room_id,
            transport,
            server_key,
            psk,
        }
    }

    /// Parse a link (same as [`str::parse`]).
    ///
    /// # Errors
    /// Any [`LinkError`]; see the module docs for the grammar.
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        let (before_fragment, fragment) = match text.split_once('#') {
            Some((before, fragment)) => (before, Some(fragment)),
            None => (text, None),
        };
        let (scheme, rest) = split_scheme(before_fragment)?;
        let secure = match scheme {
            SECURE_SCHEME => true,
            INSECURE_SCHEME => false,
            other => return Err(LinkError::UnknownScheme(other.to_owned())),
        };
        let (path_part, query) = match rest.split_once('?') {
            Some((path, query)) => (path, query),
            None => (rest, ""),
        };
        let (authority, path) = path_part.split_at(path_part.find('/').unwrap_or(path_part.len()));
        let (host, port) = split_authority(authority)?;
        let host = parse_host(host)?;

        let path = path.strip_prefix('/').unwrap_or(path);
        if path.is_empty() {
            return Err(LinkError::MissingRoom);
        }
        let (prefix, room) = match path.rsplit_once('/') {
            Some((prefix, room)) => (prefix, room),
            None => ("", path),
        };
        if room.is_empty() {
            return Err(LinkError::InvalidPath(path.to_owned()));
        }
        let prefix = parse_prefix(prefix)?;
        let room_id = RoomId::parse(room)?;
        let transport = parse_query(query)?;

        let fragment = match fragment {
            Some(fragment) if !fragment.is_empty() => fragment,
            _ => return Err(LinkError::MissingFragment),
        };
        let mut parts = fragment.split('.');
        let (Some(key), Some(psk), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(LinkError::InvalidFragment);
        };
        let server_key = decode_key(key, KeyField::ServerKey)?;
        let psk = decode_key(psk, KeyField::Psk)?;

        Ok(RelayLink {
            address: RelayAddress {
                secure,
                host,
                port: port.unwrap_or(default_port(secure)),
                prefix,
            },
            room_id,
            transport,
            server_key,
            psk,
        })
    }

    /// The relay address.
    #[must_use]
    pub fn address(&self) -> &RelayAddress {
        &self.address
    }

    /// The room id.
    #[must_use]
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// The transport binding hint.
    #[must_use]
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// The backend's Noise static public key (X25519).
    #[must_use]
    pub fn server_key(&self) -> &[u8; 32] {
        &self.server_key
    }

    /// The pre-shared key.
    #[must_use]
    pub fn psk(&self) -> &[u8; 32] {
        &self.psk
    }

    /// The full, canonical link including the secret fragment. Treat the
    /// result as a credential: never log it.
    #[must_use]
    pub fn to_secret_string(&self) -> String {
        let mut out = self.redacted();
        if self.transport != Transport::Auto {
            out.push_str("?t=");
            out.push_str(self.transport.as_str());
        }
        out.push('#');
        out.push_str(&URL_SAFE_NO_PAD.encode(self.server_key));
        out.push('.');
        out.push_str(&URL_SAFE_NO_PAD.encode(self.psk));
        out
    }

    /// The link without query or secret fragment:
    /// `hya[+insecure]://host[:port][/prefix]/<room_id>`. Safe to log and
    /// display.
    #[must_use]
    pub fn redacted(&self) -> String {
        format!(
            "{}://{}{}/{}",
            self.address.link_scheme(),
            self.address.authority(),
            self.address.prefix,
            self.room_id
        )
    }
}

impl FromStr for RelayLink {
    type Err = LinkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        RelayLink::parse(text)
    }
}

/// Prints [`RelayLink::redacted`], never the keys.
impl fmt::Display for RelayLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl fmt::Debug for RelayLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayLink")
            .field("address", &self.address)
            .field("room_id", &self.room_id)
            .field("transport", &self.transport)
            .field("server_key", &"<redacted>")
            .field("psk", &"<redacted>")
            .finish()
    }
}

fn default_port(secure: bool) -> u16 {
    if secure {
        DEFAULT_SECURE_PORT
    } else {
        DEFAULT_INSECURE_PORT
    }
}

/// Split `scheme://rest`; the scheme is reported without the rest so key
/// material can never end up in an error.
fn split_scheme(text: &str) -> Result<(&str, &str), LinkError> {
    match text.split_once("://") {
        Some((scheme, rest)) => Ok((scheme, rest)),
        None => {
            let scheme = text.split_once(':').map_or("", |(scheme, _)| scheme);
            Err(LinkError::UnknownScheme(scheme.to_owned()))
        }
    }
}

/// Split `host[:port]` (IPv6 hosts are bracketed).
fn split_authority(authority: &str) -> Result<(&str, Option<u16>), LinkError> {
    let (host, port) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => {
                let (host, rest) = authority.split_at(end + 1);
                if rest.is_empty() {
                    (host, None)
                } else if let Some(port) = rest.strip_prefix(':') {
                    (host, Some(port))
                } else {
                    return Err(LinkError::InvalidHost(authority.to_owned()));
                }
            }
            None => return Err(LinkError::InvalidHost(authority.to_owned())),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        None => None,
        Some(text) => {
            let valid = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
            match text.parse::<u16>() {
                Ok(port) if valid && port != 0 => Some(port),
                _ => return Err(LinkError::InvalidPort(text.to_owned())),
            }
        }
    };
    Ok((host, port))
}

/// Validate and lowercase a host: DNS name / IPv4, or `[IPv6]`.
fn parse_host(host: &str) -> Result<String, LinkError> {
    let valid = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !inner.is_empty()
            && inner.contains(':')
            && inner
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
    } else {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
    };
    if valid {
        Ok(host.to_ascii_lowercase())
    } else {
        Err(LinkError::InvalidHost(host.to_owned()))
    }
}

/// Validate a path prefix (with or without a leading `/`) and normalize it
/// to `""` or `/seg[/seg…]`.
fn parse_prefix(prefix: &str) -> Result<String, LinkError> {
    let trimmed = prefix.strip_prefix('/').unwrap_or(prefix);
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::with_capacity(trimmed.len() + 1);
    for segment in trimmed.split('/') {
        let valid = !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
        if !valid {
            return Err(LinkError::InvalidPath(prefix.to_owned()));
        }
        out.push('/');
        out.push_str(segment);
    }
    Ok(out)
}

/// Read `t` from the query; other parameters are ignored.
fn parse_query(query: &str) -> Result<Transport, LinkError> {
    let mut transport = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "t" {
            if transport.is_some() {
                return Err(LinkError::InvalidQuery("t given more than once".to_owned()));
            }
            transport = Some(value.parse::<Transport>()?);
        }
    }
    Ok(transport.unwrap_or_default())
}

fn decode_key(text: &str, field: KeyField) -> Result<[u8; 32], LinkError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| LinkError::InvalidBase64 { field })?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| LinkError::InvalidKeyLength {
        field,
        len: bytes.len(),
    })
}
