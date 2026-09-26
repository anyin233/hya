//! The client bridge of the secure relay (docs/relay.md "Connecting from a
//! client", ADR-0025): `hya bridge <link>` and the in-process bridge of bare
//! `hya --connect <link>`.
//!
//! The bridge listens on a loopback address and turns every accepted TCP
//! connection into one relay stream carrying one Noise tunnel to the remote
//! backend (`RelayClient::open`, then `NoiseStream::initiate_link`), then
//! splices the two. The TUI keeps its plain HTTP/SSE/WebSocket client and
//! points `--server` at the bridge; all crypto stays here.
//!
//! - A tunnel that fails its integrity checks (tampered or truncated records)
//!   resets the TCP connection (`SO_LINGER 0`) instead of closing it, so a
//!   cut response never looks complete.
//! - When no tunnel can be opened (the backend is offline, or the link was
//!   rotated or is wrong, which the proxy answers like an offline room; the
//!   relay is unreachable; the backend rejected the handshake) an
//!   authenticated HTTP request gets a
//!   `503 {"error":{"code":"unavailable","message":…}}` answer, the error
//!   envelope of the hya server, so health probes and error displays make
//!   sense.
//! - The link is the credential: it is read from an argument, stdin (`-`),
//!   or `HYA_RELAY_LINK` (removed from the environment once read), and only
//!   its redacted form is ever printed.
//! - The bridge has its own credential: a random 256-bit token made at start
//!   ([`Bridge::token`]; the `--json` readiness line's `token`, `HYA_SERVER_TOKEN`
//!   for bare `hya --connect`'s TUIs). The first HTTP request of every TCP
//!   connection must carry it as `x-hya-bridge-token: <token>`; without it
//!   the connection gets `401 {"error":{"code":"unauthenticated",…}}` and no
//!   relay stream is opened. The header is removed before the request enters
//!   the tunnel. The bridge splices bytes after that first request, so later
//!   requests on an authenticated keep-alive connection (and an upgraded
//!   WebSocket) are trusted as the same client — only the peer that sent the
//!   token can write on that connection. The token is never logged.
//!
//! Dispatched before any runtime composition, like `hya proxy`: no config,
//! providers, or database.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use hya_relay::client::{ClientConfig, ClientError, RelayClient};
use hya_relay::link::{RelayLink, Transport};
use hya_relay::proto::RelayErrorCode;
use hya_relay::transport::ChunkTransport;
use hya_relay::tunnel::{NoiseStream, TunnelConfig, TunnelError};
use serde::Serialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Environment variable holding a relay link (instead of an argument).
pub(crate) const LINK_ENV: &str = "HYA_RELAY_LINK";
/// Request header carrying the bridge token.
pub(crate) const TOKEN_HEADER: &str = "x-hya-bridge-token";
/// Environment variable through which bare `hya --connect` hands the bridge
/// token to its TUIs (never argv: process listings).
pub(crate) const TOKEN_ENV: &str = "HYA_SERVER_TOKEN";
/// How long a new connection may take to send its first request head.
const FIRST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default `--listen`: a free loopback port.
pub(crate) const DEFAULT_LISTEN: &str = "127.0.0.1:0";
/// Deadline of the Noise handshake after the relay opened the stream.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Most bytes of a first request head that are read.
const REQUEST_HEAD_LIMIT: usize = 16 * 1024;
/// How long a refused connection is drained after the answer.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How long open connections may finish after a stop was requested.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// `hya bridge` flags.
#[derive(clap::Args, Debug, Clone)]
pub(crate) struct BridgeArgs {
    /// The relay link (`hya://…#…`, the credential). `-` reads it from stdin
    /// (recommended: an argument is visible in process listings); without
    /// it, `$HYA_RELAY_LINK`.
    #[arg(value_name = "LINK")]
    pub(crate) link: Option<String>,
    /// Loopback address to listen on (`127.0.0.1:PORT`, `[::1]:PORT`,
    /// `localhost:PORT`, or a port). Other addresses are refused: the
    /// bridge gives full control of the remote backend to anyone who can
    /// connect to it.
    #[arg(long, default_value = DEFAULT_LISTEN, value_name = "ADDR")]
    pub(crate) listen: String,
    /// Extra trusted CA certificates (PEM) for a relay behind a private CA.
    #[arg(long, value_name = "PEM")]
    pub(crate) relay_ca: Option<PathBuf>,
    /// Relay binding, overriding the link's `t=`.
    #[arg(long, value_parser = ["auto", "grpc", "ws"])]
    pub(crate) transport: Option<String>,
    /// Print one JSON line `{"url","room","proxy","label","token"}` to
    /// stdout once listening (for a parent process), instead of the plain
    /// lines. `token` is the bridge token every connection must send.
    #[arg(long)]
    pub(crate) json: bool,
    /// Exit when stdin reaches end of file: a parent process that holds the
    /// pipe takes the bridge down with it.
    #[arg(long)]
    pub(crate) exit_with_stdin: bool,
}

/// Where a relay link comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkSource {
    /// Given on the command line (visible in process listings).
    Arg(String),
    /// `-`: one line from stdin (a hidden prompt on a terminal).
    Stdin,
    /// `$HYA_RELAY_LINK`.
    Env,
}

impl LinkSource {
    /// `-` → stdin, empty or missing → the environment, else the argument.
    pub(crate) fn from_arg(arg: Option<&str>) -> Self {
        match arg.map(str::trim) {
            Some("-") => LinkSource::Stdin,
            None | Some("") => LinkSource::Env,
            Some(link) => LinkSource::Arg(link.to_owned()),
        }
    }
}

/// Read and parse the link. `flag` names the option in messages
/// (`hya bridge`, `--connect`).
pub(crate) fn read_link(source: &LinkSource, flag: &str) -> anyhow::Result<RelayLink> {
    let from_env = take_link_env();
    let text = match source {
        LinkSource::Arg(text) => text.clone(),
        LinkSource::Env => from_env
            .filter(|value| !value.trim().is_empty())
            .with_context(|| {
                format!(
                    "{flag}: no relay link: pass `-` to read it from stdin (recommended), set {LINK_ENV}, or give it as an argument"
                )
            })?,
        LinkSource::Stdin => read_stdin_line()?,
    };
    parse_link(&text).map_err(|error| anyhow::anyhow!("{flag}: {error}"))
}

/// Read `HYA_RELAY_LINK` and remove it from this process's environment, so
/// no child process (the daemon, the TUIs, the web host, a shell) inherits
/// the credential. Children are also spawned with it removed explicitly.
pub(crate) fn take_link_env() -> Option<String> {
    let value = std::env::var(LINK_ENV).ok();
    if std::env::var_os(LINK_ENV).is_some() {
        // SAFETY: called on the main task while the command line is being
        // handled, before this process starts its own tasks or children;
        // std's environment accessors are serialized by its lock, and no
        // foreign code reads the environment concurrently at this point.
        unsafe { std::env::remove_var(LINK_ENV) };
    }
    value
}

/// A new bridge token: 32 random bytes from the OS, lowercase hex.
pub(crate) fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    // SAFETY: `getentropy` writes at most 256 bytes into the valid, writable
    // 32-byte buffer it is given.
    let status = unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) };
    if status != 0 {
        anyhow::bail!(
            "could not make the bridge token: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether `a` and `b` are equal, in time independent of where they differ.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Check the first request head (`head`, up to and including the blank
/// line) for the bridge token and return it without the token header
/// lines; `None` when no token header is present or any one is wrong.
pub(crate) fn authorize(head: &[u8], token: &str) -> Option<Vec<u8>> {
    let mut kept = Vec::with_capacity(head.len());
    let mut found = false;
    let mut lines = head.split_inclusive(|byte| *byte == b'\n');
    // The request line.
    kept.extend_from_slice(lines.next()?);
    for line in lines {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if let Some(colon) = text.iter().position(|byte| *byte == b':')
            && text[..colon].eq_ignore_ascii_case(TOKEN_HEADER.as_bytes())
        {
            let value = text[colon + 1..].trim_ascii();
            if !same_secret(value, token.as_bytes()) {
                return None;
            }
            found = true;
            continue;
        }
        kept.extend_from_slice(line);
    }
    found.then_some(kept)
}

/// Parse a link; errors never contain key material.
pub(crate) fn parse_link(text: &str) -> Result<RelayLink, String> {
    RelayLink::parse(text.trim()).map_err(|error| format!("invalid relay link: {error}"))
}

/// Whether a link given this way deserves the process-listing warning.
pub(crate) fn exposed_in_argv(source: &LinkSource) -> bool {
    matches!(source, LinkSource::Arg(_))
}

/// The warning printed for a link given as an argument.
pub(crate) const ARGV_WARNING: &str = "the relay link was given as an argument, so it is visible in process listings; pass `-` and write it to stdin, or set HYA_RELAY_LINK";

/// One line from stdin; on a terminal, prompted with echo turned off.
fn read_stdin_line() -> anyhow::Result<String> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.is_terminal() {
        eprint!("Relay link (input hidden): ");
        let _ = std::io::stderr().flush();
        let echo = EchoOff::new();
        let read = stdin.lock().read_line(&mut line);
        drop(echo);
        eprintln!();
        read.context("read the relay link")?;
    } else {
        stdin
            .lock()
            .read_line(&mut line)
            .context("read the relay link from stdin")?;
    }
    if line.trim().is_empty() {
        anyhow::bail!("no relay link on stdin");
    }
    Ok(line)
}

/// Terminal echo off for its lifetime (best effort).
struct EchoOff(Option<libc::termios>);

impl EchoOff {
    fn new() -> Self {
        // SAFETY: `termios` is plain data; `tcgetattr`/`tcsetattr` only read
        // and write it for fd 0, which is open (it is our stdin).
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return Self(None);
            }
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) != 0 {
                return Self(None);
            }
            Self(Some(saved))
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(saved) = self.0 {
            // SAFETY: restores the attributes read in `new` on the same fd.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
    }
}

/// Parse `--listen`: a socket address, `localhost:PORT`, or a bare port
/// (on 127.0.0.1). The address must be loopback.
pub(crate) fn parse_listen(text: &str) -> anyhow::Result<SocketAddr> {
    let text = text.trim();
    let addr = if let Ok(addr) = text.parse::<SocketAddr>() {
        addr
    } else if let Ok(port) = text.parse::<u16>() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    } else if let Some(port) = text
        .strip_prefix("localhost:")
        .and_then(|port| port.parse::<u16>().ok())
    {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    } else {
        anyhow::bail!("--listen {text:?}: expected 127.0.0.1:PORT, [::1]:PORT, or a port");
    };
    check_loopback(addr)?;
    Ok(addr)
}

/// Refuse non-loopback addresses: whoever reaches the bridge controls the
/// remote backend, with no further authentication.
pub(crate) fn check_loopback(addr: SocketAddr) -> anyhow::Result<()> {
    if addr.ip().is_loopback() {
        return Ok(());
    }
    anyhow::bail!(
        "--listen {addr} is not a loopback address: anyone who reaches the bridge controls the remote backend, so it only listens on 127.0.0.1 or ::1"
    )
}

/// `link` with its transport replaced by `transport` (`--transport`).
pub(crate) fn with_transport(link: RelayLink, transport: Option<Transport>) -> RelayLink {
    match transport {
        Some(transport) if transport != link.transport() => RelayLink::new(
            link.address().clone(),
            link.room_id().clone(),
            transport,
            *link.server_key(),
            *link.psk(),
        ),
        _ => link,
    }
}

/// `--transport auto|grpc|ws`.
pub(crate) fn parse_transport(text: &str) -> anyhow::Result<Transport> {
    match text {
        "auto" => Ok(Transport::Auto),
        "grpc" => Ok(Transport::Grpc),
        "ws" => Ok(Transport::Ws),
        other => anyhow::bail!("--transport {other:?}: expected auto, grpc, or ws"),
    }
}

/// The relay of `link` without its room: `hya[+insecure]://host[:port][/prefix]`.
pub(crate) fn proxy_text(link: &RelayLink) -> String {
    let redacted = link.redacted();
    redacted
        .rsplit_once('/')
        .map_or(redacted.clone(), |(proxy, _room)| proxy.to_owned())
}

/// What the TUI shows instead of the loopback URL: `remote: host[:port][/prefix]/<room>`.
pub(crate) fn remote_label(link: &RelayLink) -> String {
    let address = link.address();
    format!(
        "remote: {}{}/{}",
        address.authority(),
        address.prefix(),
        link.room_id()
    )
}

/// `--json` readiness line.
#[derive(Serialize)]
struct Ready<'a> {
    url: &'a str,
    room: &'a str,
    proxy: &'a str,
    label: &'a str,
    token: &'a str,
}

/// Why no tunnel opened when the proxy says the room is not there. The
/// proxy answers a wrong or rotated link's open token exactly like a room
/// without a host, so the bridge cannot tell the two apart.
pub(crate) const OFFLINE_MESSAGE: &str = "remote backend is offline, or the relay link was rotated or is wrong (ask for a new link: `hya serve relay link`)";

/// The HTTP answer to a request that cannot reach the remote backend.
pub(crate) fn unavailable_response(message: &str) -> String {
    error_response("503 Service Unavailable", "unavailable", message)
}

/// The message of the `401` answer to a connection without the token.
pub(crate) const UNAUTHENTICATED_MESSAGE: &str = "the hya bridge needs its token: send the x-hya-bridge-token header (the token is the `token` of `hya bridge --json`, or HYA_SERVER_TOKEN for the TUIs of bare `hya --connect`)";

/// The HTTP answer to a connection whose first request lacks the token.
pub(crate) fn unauthenticated_response() -> String {
    error_response(
        "401 Unauthorized",
        "unauthenticated",
        UNAUTHENTICATED_MESSAGE,
    )
}

/// An HTTP/1.1 answer with the hya server's error envelope; the connection
/// closes after it.
fn error_response(status: &str, code: &str, message: &str) -> String {
    let body = serde_json::json!({"error": {"code": code, "message": message}}).to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Whether `head` starts like an HTTP/1.x request line (`METHOD `).
pub(crate) fn looks_like_http(head: &[u8]) -> bool {
    const METHODS: [&[u8]; 9] = [
        b"GET ",
        b"POST ",
        b"PUT ",
        b"DELETE ",
        b"HEAD ",
        b"OPTIONS ",
        b"PATCH ",
        b"CONNECT ",
        b"TRACE ",
    ];
    METHODS.iter().any(|method| head.starts_with(method))
}

/// Where status lines go (stderr for the CLI; the log file for bare `hya`).
pub(crate) type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Status lines on stderr, prefixed `hya bridge:`. Text that came from the
/// relay (an error message) is stripped of terminal controls.
pub(crate) fn stderr_log() -> Log {
    Arc::new(|line| eprintln!("hya bridge: {}", hya_server::display_text(line)))
}

/// Settings of a [`Bridge`].
#[derive(Clone, Debug)]
pub(crate) struct BridgeOptions {
    /// Loopback listen address.
    pub(crate) listen: SocketAddr,
    /// Relay client settings (CA file, binding pin, timeouts).
    pub(crate) client: ClientConfig,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            client: ClientConfig::default(),
        }
    }
}

/// The remote backend as the bridge last saw it; status lines are printed
/// on changes only.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Health {
    Unknown = 0,
    Online = 1,
    Offline = 2,
    Unreachable = 3,
    Rejected = 4,
}

impl Health {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Health::Online,
            2 => Health::Offline,
            3 => Health::Unreachable,
            4 => Health::Rejected,
            _ => Health::Unknown,
        }
    }
}

/// Why no tunnel could be opened.
enum OpenError {
    /// The room has no host.
    Offline,
    /// The relay failed or is unreachable.
    Relay(ClientError),
    /// The backend refused the link (wrong or rotated PSK / key).
    Rejected,
    /// The handshake did not finish in time.
    Timeout,
    /// The tunnel failed otherwise during the handshake.
    Tunnel(TunnelError),
}

impl OpenError {
    /// The message of the 503 answer (no secrets: client and tunnel errors
    /// never contain key material).
    fn message(&self) -> String {
        match self {
            OpenError::Offline => OFFLINE_MESSAGE.to_owned(),
            OpenError::Relay(error) => format!("the relay is unreachable: {error}"),
            OpenError::Rejected => {
                "the remote backend rejected the relay link (rotated or wrong link)".to_owned()
            }
            OpenError::Timeout => {
                "the remote backend did not finish the tunnel handshake in time".to_owned()
            }
            OpenError::Tunnel(error) => format!("the tunnel to the remote backend failed: {error}"),
        }
    }

    fn health(&self) -> Health {
        match self {
            OpenError::Offline => Health::Offline,
            OpenError::Rejected => Health::Rejected,
            _ => Health::Unreachable,
        }
    }
}

struct Shared {
    client: RelayClient,
    link: RelayLink,
    log: Log,
    health: AtomicU8,
    /// The bridge token (never logged).
    token: String,
    /// A refused connection was logged already (only the first one is).
    refusal_logged: std::sync::atomic::AtomicBool,
}

impl Shared {
    /// Open a relay stream to the room and run the Noise handshake on it.
    async fn open(&self) -> Result<NoiseStream<ChunkTransport>, OpenError> {
        let leg = match self.client.open(self.link.room_id()).await {
            Ok(leg) => leg,
            Err(ClientError::RoomOffline(_)) => return Err(OpenError::Offline),
            Err(error) => {
                if matches!(
                    error,
                    ClientError::Connect { .. }
                        | ClientError::NoBinding { .. }
                        | ClientError::Transport(_)
                ) {
                    // The path may have changed: probe again next time.
                    self.client.forget_binding();
                }
                return Err(OpenError::Relay(error));
            }
        };
        match timeout(
            HANDSHAKE_TIMEOUT,
            NoiseStream::initiate_link(leg, &self.link, TunnelConfig::default()),
        )
        .await
        {
            Err(_) => Err(OpenError::Timeout),
            Ok(Ok(tunnel)) => Ok(tunnel),
            Ok(Err(TunnelError::Handshake(_))) => Err(OpenError::Rejected),
            Ok(Err(TunnelError::Relay {
                code: RelayErrorCode::NotFound,
                ..
            })) => Err(OpenError::Offline),
            Ok(Err(error)) => Err(OpenError::Tunnel(error)),
        }
    }

    /// Record the backend's state; print a status line when it changed.
    async fn report(&self, health: Health, detail: &str) {
        let before = Health::from_u8(self.health.swap(health as u8, Ordering::SeqCst));
        if before == health {
            return;
        }
        let line = match health {
            Health::Online => {
                let binding = self
                    .client
                    .binding()
                    .await
                    .map_or_else(|_| "unknown".to_owned(), |choice| choice.binding.to_string());
                if before == Health::Unknown {
                    format!("remote backend online over the {binding} binding")
                } else {
                    format!("remote backend reachable again over the {binding} binding")
                }
            }
            Health::Offline => format!(
                "{OFFLINE_MESSAGE}; HTTP requests get 503 until it answers"
            ),
            Health::Unreachable => format!("{detail}; retrying on the next connection"),
            Health::Rejected => {
                "the remote backend rejected the relay link (rotated or wrong link); ask for a new one (`hya serve relay link`)"
                    .to_owned()
            }
            Health::Unknown => return,
        };
        (self.log)(&line);
    }
}

/// A running bridge. Dropping it stops accepting; [`Bridge::shutdown`]
/// also lets open connections finish briefly.
pub(crate) struct Bridge {
    addr: SocketAddr,
    room: String,
    proxy: String,
    label: String,
    token: String,
    stop: CancellationToken,
    hard_stop: CancellationToken,
    tasks: TaskTracker,
    accept: JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.cancel();
        self.hard_stop.cancel();
        self.accept.abort();
    }
}

impl Bridge {
    /// Listen, choose the relay binding, check the link against the
    /// backend, and start accepting.
    ///
    /// # Errors
    /// A non-loopback or busy listen address, an unreadable CA file, a relay
    /// that no binding reaches, or a backend that rejects the handshake (a
    /// link with the current PSK but a wrong server key). An offline backend
    /// is not an error (requests get 503 until it answers) — nor is a
    /// rotated or wrong PSK, which the proxy answers exactly like an offline
    /// room.
    pub(crate) async fn start(
        link: RelayLink,
        options: BridgeOptions,
        log: Log,
    ) -> anyhow::Result<Self> {
        check_loopback(options.listen)?;
        let listener = TcpListener::bind(options.listen)
            .await
            .with_context(|| format!("listen on {}", options.listen))?;
        let addr = listener.local_addr().context("read the listen address")?;
        let proxy = proxy_text(&link);
        let client = RelayClient::from_link(&link, options.client)
            .map_err(|error| anyhow::anyhow!("relay {proxy}: {error}"))?;
        let choice = client
            .binding()
            .await
            .map_err(|error| anyhow::anyhow!("cannot reach the relay {proxy}: {error}"))?;
        log(&format!(
            "relay {proxy}: {} binding ({})",
            choice.binding, choice.reason
        ));
        let token = new_token()?;
        let shared = Arc::new(Shared {
            client,
            link,
            log,
            health: AtomicU8::new(Health::Unknown as u8),
            token: token.clone(),
            refusal_logged: std::sync::atomic::AtomicBool::new(false),
        });
        // Check the link once now: a link the backend rejects is an error at
        // start, not a stream of 503s. (A rotated or wrong PSK never reaches
        // the backend: the proxy says the room is offline.)
        match shared.open().await {
            Ok(tunnel) => {
                tokio::spawn(close_quietly(tunnel));
                shared.report(Health::Online, "").await;
            }
            Err(OpenError::Rejected) => anyhow::bail!(
                "the remote backend rejected the relay link {} (rotated or wrong link); ask for a new one (`hya serve relay link`)",
                shared.link.redacted()
            ),
            Err(error) => shared.report(error.health(), &error.message()).await,
        }
        let stop = CancellationToken::new();
        let hard_stop = CancellationToken::new();
        let tasks = TaskTracker::new();
        let accept = tokio::spawn(accept_loop(
            listener,
            shared.clone(),
            stop.clone(),
            hard_stop.clone(),
            tasks.clone(),
        ));
        Ok(Self {
            addr,
            room: shared.link.room_id().to_string(),
            label: remote_label(&shared.link),
            proxy,
            token,
            stop,
            hard_stop,
            tasks,
            accept,
        })
    }

    /// `http://127.0.0.1:PORT` (or `http://[::1]:PORT`).
    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `remote: host[:port][/prefix]/<room>`.
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    /// The bridge token every connection's first request must carry
    /// (`x-hya-bridge-token`). A secret: never log it.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The `--json` readiness line (holds the token).
    pub(crate) fn ready_json(&self) -> String {
        let url = self.url();
        serde_json::to_string(&Ready {
            url: &url,
            room: &self.room,
            proxy: &self.proxy,
            label: &self.label,
            token: &self.token,
        })
        .unwrap_or_default()
    }

    /// Stop accepting, give open connections [`SHUTDOWN_GRACE`], then cut them.
    pub(crate) async fn shutdown(self) {
        self.stop.cancel();
        self.tasks.close();
        if timeout(SHUTDOWN_GRACE, self.tasks.wait()).await.is_err() {
            self.hard_stop.cancel();
            let _ = timeout(SHUTDOWN_GRACE, self.tasks.wait()).await;
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<Shared>,
    stop: CancellationToken,
    hard_stop: CancellationToken,
    tasks: TaskTracker,
) {
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let tcp = match accepted {
            Ok((tcp, _)) => tcp,
            Err(_) => {
                // Out of descriptors or similar: do not spin.
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let shared = shared.clone();
        let hard_stop = hard_stop.clone();
        tasks.spawn(async move {
            tokio::select! {
                () = serve_connection(tcp, &shared) => {}
                () = hard_stop.cancelled() => {}
            }
        });
    }
}

/// One client connection: its first request is checked for the token, then
/// a tunnel is spliced to it; or a refusal.
async fn serve_connection(mut tcp: TcpStream, shared: &Shared) {
    let _ = tcp.set_nodelay(true);
    let received = read_head(&mut tcp).await;
    if !looks_like_http(&received) {
        reset(tcp);
        return;
    }
    let authorized = received
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .and_then(|end| {
            let (head, rest) = received.split_at(end + 4);
            authorize(head, &shared.token).map(|mut first| {
                first.extend_from_slice(rest);
                first
            })
        });
    let Some(first) = authorized else {
        if !shared
            .refusal_logged
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            (shared.log)(
                "refused a connection without the bridge token (401; further refusals are not logged)",
            );
        }
        answer(tcp, &unauthenticated_response()).await;
        return;
    };
    let mut tunnel = match shared.open().await {
        Ok(tunnel) => tunnel,
        Err(error) => {
            // Relay errors may carry the relay's text: no terminal controls.
            let message = hya_server::display_text(&error.message());
            shared.report(error.health(), &message).await;
            answer(tcp, &unavailable_response(&message)).await;
            return;
        }
    };
    shared.report(Health::Online, "").await;
    if tunnel.write_all(&first).await.is_err() {
        reset(tcp);
        return;
    }
    if let Err(error) = tokio::io::copy_bidirectional(&mut tcp, &mut tunnel).await {
        let integrity = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<TunnelError>())
            .is_some_and(|inner| matches!(inner, TunnelError::Decrypt | TunnelError::Truncated));
        if integrity {
            (shared.log)(&format!(
                "a response from the remote backend failed its integrity check ({error}); the connection was reset"
            ));
        }
        // Never a clean close: the client must not take a cut or forged
        // response for a complete one. Dropping the tunnel aborts its stream.
        reset(tcp);
    }
}

/// Close the connection with a TCP reset (`SO_LINGER {on, 0}`, then close).
fn reset(tcp: TcpStream) {
    use std::os::fd::AsRawFd as _;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: `setsockopt` reads `size_of::<linger>()` bytes from a valid
    // `linger` on the open socket descriptor `tcp` owns.
    unsafe {
        libc::setsockopt(
            tcp.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::from_ref(&linger).cast(),
            libc::socklen_t::try_from(std::mem::size_of::<libc::linger>()).unwrap_or(8),
        );
    }
    drop(tcp);
}

/// Answer an HTTP request (its head already read) with `response`, then
/// close.
async fn answer(mut tcp: TcpStream, response: &str) {
    if tcp.write_all(response.as_bytes()).await.is_err() {
        reset(tcp);
        return;
    }
    let _ = tcp.shutdown().await;
    // Read what the client still sends (a request body) so the close is a
    // FIN, not a reset that could discard the answer.
    let _ = timeout(DRAIN_TIMEOUT, async {
        let mut sink = [0u8; 4096];
        while let Ok(n) = tcp.read(&mut sink).await {
            if n == 0 {
                break;
            }
        }
    })
    .await;
}

/// The first request head (up to the blank line, a limit, EOF, or a
/// timeout), with whatever else arrived in the same reads.
async fn read_head(tcp: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let _ = timeout(FIRST_REQUEST_TIMEOUT, async {
        let mut buf = [0u8; 2048];
        while head.len() < REQUEST_HEAD_LIMIT && !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match tcp.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
    })
    .await;
    head
}

/// End the start-up check tunnel politely: the backend sees EOF, not an
/// aborted stream.
async fn close_quietly(mut tunnel: NoiseStream<ChunkTransport>) {
    let _ = timeout(Duration::from_secs(5), async {
        tunnel.shutdown().await?;
        let mut sink = Vec::new();
        tunnel.read_to_end(&mut sink).await
    })
    .await;
}

/// `hya bridge`.
pub(crate) async fn cmd_bridge(args: BridgeArgs) -> anyhow::Result<()> {
    let listen = parse_listen(&args.listen)?;
    let transport = args.transport.as_deref().map(parse_transport).transpose()?;
    let log = stderr_log();
    let source = LinkSource::from_arg(args.link.as_deref());
    if exposed_in_argv(&source) {
        log(ARGV_WARNING);
    }
    let link = with_transport(read_link(&source, "hya bridge")?, transport);
    let mut signals = StopSignals::install().context("install signal handlers")?;
    let options = BridgeOptions {
        listen,
        client: ClientConfig {
            extra_ca_pem: args.relay_ca,
            ..ClientConfig::default()
        },
    };
    let bridge = Bridge::start(link, options, log.clone()).await?;
    // The token goes to stdout only (the caller's channel), never to the
    // status log.
    if args.json {
        println!("{}", bridge.ready_json());
    } else {
        println!("hya bridge listening on {}", bridge.url());
        println!("hya bridge token {}", bridge.token());
        log(&format!(
            "{}; send the token as the {TOKEN_HEADER} header on every request (HYA_SERVER_TOKEN=<token> for the TUI); Ctrl+C stops the bridge",
            bridge.label()
        ));
    }
    let _ = std::io::stdout().flush();
    let stdin_closed = CancellationToken::new();
    if args.exit_with_stdin {
        let closed = stdin_closed.clone();
        // A blocking reader: std's stdin keeps whatever the link line left
        // buffered, so this sees the same stream.
        std::thread::spawn(move || {
            let mut sink = [0u8; 256];
            loop {
                match std::io::Read::read(&mut std::io::stdin(), &mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            closed.cancel();
        });
    }
    tokio::select! {
        signal = signals.recv() => log(&format!("signal {signal}; stopping")),
        () = stdin_closed.cancelled() => log("stdin closed; stopping"),
    }
    bridge.shutdown().await;
    Ok(())
}

/// SIGINT, SIGTERM, and SIGHUP.
struct StopSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.interrupt.recv() => libc::SIGINT,
            _ = self.terminate.recv() => libc::SIGTERM,
            _ = self.hangup.recv() => libc::SIGHUP,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn link(text_query: &str) -> RelayLink {
        let key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
        let psk = "__________________________________________8";
        RelayLink::parse(&format!(
            "hya://relay.example.com/hya/eh7ddx5bksrgcytl7bkai36se4{text_query}#{key}.{psk}"
        ))
        .unwrap()
    }

    #[test]
    fn listen_accepts_loopback_forms_only() {
        assert_eq!(
            parse_listen("127.0.0.1:0").unwrap().to_string(),
            "127.0.0.1:0"
        );
        assert_eq!(
            parse_listen("[::1]:4000").unwrap().to_string(),
            "[::1]:4000"
        );
        assert_eq!(
            parse_listen("localhost:4001").unwrap().to_string(),
            "127.0.0.1:4001"
        );
        assert_eq!(parse_listen("4002").unwrap().to_string(), "127.0.0.1:4002");
        for refused in ["0.0.0.0:0", "[::]:0", "192.168.1.5:80", "10.0.0.1:1"] {
            let error = parse_listen(refused).unwrap_err().to_string();
            assert!(error.contains("loopback"), "{refused}: {error}");
        }
        assert!(parse_listen("example.com:80").is_err());
    }

    #[test]
    fn link_sources() {
        assert_eq!(LinkSource::from_arg(Some("-")), LinkSource::Stdin);
        assert_eq!(LinkSource::from_arg(None), LinkSource::Env);
        assert_eq!(LinkSource::from_arg(Some("")), LinkSource::Env);
        assert_eq!(
            LinkSource::from_arg(Some("hya://x")),
            LinkSource::Arg("hya://x".into())
        );
        assert!(exposed_in_argv(&LinkSource::Arg("x".into())));
        assert!(!exposed_in_argv(&LinkSource::Stdin));
        assert!(!exposed_in_argv(&LinkSource::Env));
    }

    #[test]
    fn invalid_links_never_echo_the_secret() {
        let error = parse_link(
            "hya://relay.example.com/hya/eh7ddx5bksrgcytl7bkai36se4#SECRETKEY.SECRETPSK",
        )
        .unwrap_err();
        assert!(error.starts_with("invalid relay link"), "{error}");
        assert!(!error.contains("SECRET"), "{error}");
    }

    #[test]
    fn transport_override_replaces_the_link_hint() {
        let pinned = link("?t=grpc");
        assert_eq!(
            with_transport(pinned.clone(), None).transport(),
            Transport::Grpc
        );
        let auto = with_transport(pinned.clone(), Some(Transport::Auto));
        assert_eq!(auto.transport(), Transport::Auto);
        assert_eq!(auto.psk(), pinned.psk());
        assert_eq!(auto.server_key(), pinned.server_key());
        assert_eq!(auto.room_id(), pinned.room_id());
        assert_eq!(
            with_transport(pinned, Some(Transport::Ws)).transport(),
            Transport::Ws
        );
    }

    #[test]
    fn proxy_and_label_are_redacted() {
        let link = link("");
        assert_eq!(proxy_text(&link), "hya://relay.example.com/hya");
        assert_eq!(
            remote_label(&link),
            "remote: relay.example.com/hya/eh7ddx5bksrgcytl7bkai36se4"
        );
    }

    #[test]
    fn the_503_answer_is_the_server_error_envelope() {
        let text = unavailable_response(OFFLINE_MESSAGE);
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{head}"
        );
        assert!(head.contains("content-type: application/json"), "{head}");
        assert!(
            head.contains(&format!("content-length: {}", body.len())),
            "{head}"
        );
        assert!(head.contains("connection: close"), "{head}");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": {"code": "unavailable", "message": OFFLINE_MESSAGE}})
        );
    }

    #[test]
    fn tokens_are_random_256_bit_hex() {
        let one = new_token().unwrap();
        let two = new_token().unwrap();
        assert_eq!(one.len(), 64);
        assert!(
            one.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_ne!(one, two);
    }

    #[test]
    fn the_first_request_must_carry_the_token_which_is_stripped() {
        let token = "ab".repeat(32);
        let head = format!(
            "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:9\r\nX-Hya-Bridge-Token:  {token} \r\naccept: */*\r\n\r\n"
        );
        let kept = authorize(head.as_bytes(), &token).unwrap();
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:9\r\naccept: */*\r\n\r\n"
        );
        // Missing, wrong, or one wrong among several.
        let missing = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(authorize(missing.as_bytes(), &token).is_none());
        let wrong = format!(
            "GET / HTTP/1.1\r\n{TOKEN_HEADER}: {}\r\n\r\n",
            "cd".repeat(32)
        );
        assert!(authorize(wrong.as_bytes(), &token).is_none());
        let short = format!("GET / HTTP/1.1\r\n{TOKEN_HEADER}: ab\r\n\r\n");
        assert!(authorize(short.as_bytes(), &token).is_none());
        let mixed =
            format!("GET / HTTP/1.1\r\n{TOKEN_HEADER}: {token}\r\n{TOKEN_HEADER}: nope\r\n\r\n");
        assert!(authorize(mixed.as_bytes(), &token).is_none());
        // Another header that merely contains the name is not the token.
        let lookalike = format!("GET / HTTP/1.1\r\nx-note: {TOKEN_HEADER}: {token}\r\n\r\n");
        assert!(authorize(lookalike.as_bytes(), &token).is_none());
    }

    #[test]
    fn the_401_answer_is_the_error_envelope_without_the_token() {
        let text = unauthenticated_response();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 401 Unauthorized\r\n"), "{head}");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["error"]["code"], "unauthenticated");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains(TOKEN_HEADER)
        );
    }

    #[test]
    fn the_link_is_taken_out_of_the_environment() {
        // SAFETY: test-only; no other test of this binary reads LINK_ENV.
        unsafe { std::env::set_var(LINK_ENV, "hya://example/room#k.p") };
        assert_eq!(take_link_env().as_deref(), Some("hya://example/room#k.p"));
        assert!(std::env::var_os(LINK_ENV).is_none());
        assert_eq!(take_link_env(), None);
    }

    #[test]
    fn recognizes_http_request_lines() {
        assert!(looks_like_http(b"GET /v1/health HTTP/1.1\r\n"));
        assert!(looks_like_http(b"POST /v1/sessions HTTP/1.1\r\n"));
        assert!(!looks_like_http(b"\x16\x03\x01"));
        assert!(!looks_like_http(b"PRI * HTTP/2.0"));
        assert!(!looks_like_http(b""));
        assert!(!looks_like_http(b"GET"));
    }
}
