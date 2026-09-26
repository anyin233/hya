//! The relay host connector (ADR-0025; docs/relay.md "Hosting a backend on
//! a relay").
//!
//! A [`RelayHost`] keeps this backend registered in its room on a
//! `hya proxy`: it holds the host control stream (reconnecting with jittered
//! backoff, slower after `ALREADY_EXISTS`), registering the hash of the
//! room's open token so the proxy only lets link holders open streams (and
//! replacing it on the same stream after a PSK rotation), and for every
//! `incoming` stream it accepts the stream, answers the Noise `NKpsk0`
//! handshake as the responder (bounded by a short timeout), and serves the
//! same `/v1` router as the TCP listener over the decrypted bytes, with the
//! [`crate::Origin::Relay`] extension on each request. Handshake failures are
//! logged without key material. Streams still handshaking and streams being
//! served have separate caps, so stalled handshakes never take a serving
//! slot.
//!
//! The connector is controlled through the loopback-only `RelayControl`
//! rpcs (`hya serve relay …`) and by `hya serve --relay`.

mod conn;
pub mod identity;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use axum::Router;
use futures::{SinkExt as _, StreamExt as _};
use hya_relay::client::{
    Backoff, BindingChoice, ClientConfig, ClientError, HeartbeatConfig, ReconnectPolicy,
    RelayClient, RetryKind, register_host,
};
use hya_relay::link::{RelayAddress, RelayLink, RoomId, Transport};
use hya_relay::proto::{ProxyToHost, proxy_to_host};
use hya_relay::transport::TransportError;
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use identity::{IdentityError, RelayIdentity};

/// Default deadline of one Noise handshake on an accepted stream. The
/// client sends its hello right after `opened`, so a healthy handshake
/// takes one round trip.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// Default deadline of one room registration.
pub const DEFAULT_REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
/// Default cap on relay streams being served at once.
pub const DEFAULT_MAX_STREAMS: usize = 64;
/// Default cap on relay streams in the Noise handshake at once.
pub const DEFAULT_MAX_HANDSHAKES: usize = 16;
/// Default time open relay streams get to finish at a graceful shutdown.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Settings of the connector's host side.
#[derive(Clone, Debug)]
pub struct RelayHostConfig {
    /// Where the database's relay identity lives
    /// (`<db>.relay-identity.json`); `None` for an in-memory database,
    /// whose identity is ephemeral.
    pub identity_path: Option<PathBuf>,
    /// Heartbeat of the control and data streams (default: every 15 s,
    /// dead after 45 s of silence).
    pub heartbeat: HeartbeatConfig,
    /// Deadline of one Noise handshake.
    pub handshake_timeout: Duration,
    /// Deadline of one room registration.
    pub register_timeout: Duration,
    /// Relay streams being served (handshake done) at once; while all are
    /// taken, incoming streams are not accepted (the proxy times them out).
    pub max_streams: usize,
    /// Relay streams in the Noise handshake at once, a budget separate from
    /// [`RelayHostConfig::max_streams`]; incoming streams beyond it are not
    /// accepted.
    pub max_handshakes: usize,
    /// Reconnect schedule of the control stream.
    pub reconnect: ReconnectPolicy,
    /// How long open relay streams may finish at a graceful shutdown.
    pub shutdown_grace: Duration,
}

impl Default for RelayHostConfig {
    fn default() -> Self {
        Self {
            identity_path: None,
            heartbeat: HeartbeatConfig::default(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            register_timeout: DEFAULT_REGISTER_TIMEOUT,
            max_streams: DEFAULT_MAX_STREAMS,
            max_handshakes: DEFAULT_MAX_HANDSHAKES,
            reconnect: ReconnectPolicy::default(),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

/// What to connect to (`hya serve --relay`, `ConnectRelay`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySettings {
    /// The relay's public URL: `https://…`, `http://…`, `hya://…`, or
    /// `hya+insecure://…` (host, optional port and path prefix).
    pub proxy_url: String,
    /// The binding (`t=` in the link).
    pub transport: Transport,
    /// Extra trusted CA certificates (PEM).
    pub extra_ca: Option<PathBuf>,
    /// A throwaway identity for this connection only.
    pub ephemeral: bool,
}

impl RelaySettings {
    /// Settings for `proxy_url` with the defaults (`auto`, no extra CA,
    /// the database's identity).
    #[must_use]
    pub fn new(proxy_url: impl Into<String>) -> Self {
        Self {
            proxy_url: proxy_url.into(),
            transport: Transport::Auto,
            extra_ca: None,
            ephemeral: false,
        }
    }

    /// The relay address of [`RelaySettings::proxy_url`].
    ///
    /// # Errors
    /// [`RelayHostError::InvalidArgument`] for an unparsable URL.
    pub fn address(&self) -> Result<RelayAddress, RelayHostError> {
        parse_proxy_url(&self.proxy_url)
    }
}

/// Parse a relay URL: `https://` / `hya://` (TLS to the first hop) or
/// `http://` / `hya+insecure://` (plaintext), host, optional port, optional
/// path prefix.
///
/// # Errors
/// [`RelayHostError::InvalidArgument`].
pub fn parse_proxy_url(url: &str) -> Result<RelayAddress, RelayHostError> {
    let url = url.trim();
    let normalized = if let Some(rest) = url.strip_prefix("hya+insecure://") {
        format!("http://{rest}")
    } else if let Some(rest) = url.strip_prefix("hya://") {
        format!("https://{rest}")
    } else {
        url.to_owned()
    };
    RelayAddress::parse_proxy_url(&normalized).map_err(|error| {
        RelayHostError::InvalidArgument(format!(
            "invalid relay URL `{url}` ({error}); expected https://host[:port][/prefix] or http://…"
        ))
    })
}

/// A failed relay-control operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayHostError {
    /// Bad input (URL, transport, CA file).
    InvalidArgument(String),
    /// The operation needs a state the connector is not in.
    FailedPrecondition(String),
    /// The identity could not be loaded or saved.
    Identity(String),
    /// The server is shutting down.
    Stopping,
}

impl std::fmt::Display for RelayHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayHostError::InvalidArgument(message)
            | RelayHostError::FailedPrecondition(message) => f.write_str(message),
            RelayHostError::Identity(message) => write!(f, "relay identity: {message}"),
            RelayHostError::Stopping => f.write_str("the server is shutting down"),
        }
    }
}

impl std::error::Error for RelayHostError {}

/// The connector's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayState {
    /// Not joined to a relay.
    Disconnected,
    /// Opening the control stream or registering.
    Connecting,
    /// Registered: link holders can connect.
    Connected,
    /// Waiting before the next attempt after a failure.
    Backoff,
}

impl RelayState {
    /// `disconnected`, `connecting`, `connected`, or `backoff`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RelayState::Disconnected => "disconnected",
            RelayState::Connecting => "connecting",
            RelayState::Connected => "connected",
            RelayState::Backoff => "backoff",
        }
    }
}

/// A snapshot of the connector. Never holds key material.
#[derive(Clone, Debug)]
pub struct RelayStatus {
    /// State.
    pub state: RelayState,
    /// The relay's public base URL (`https://host[:port][/prefix]`).
    pub proxy: Option<String>,
    /// This backend's room.
    pub room_id: Option<String>,
    /// The link without its secret fragment.
    pub redacted_link: Option<String>,
    /// The configured binding.
    pub transport: Option<Transport>,
    /// The binding in use and why.
    pub binding: Option<BindingChoice>,
    /// The last failure (cleared by a registration).
    pub last_error: Option<String>,
    /// When the room was registered.
    pub connected_since: Option<SystemTime>,
    /// Relay streams being served.
    pub active_streams: usize,
    /// Whether the identity is a throwaway one.
    pub ephemeral: bool,
}

/// Called with the new settings after a connect (`Some`) or a disconnect
/// (`None`), so the backend can record them (the discovery file).
pub type SettingsHook = Arc<dyn Fn(Option<&RelaySettings>) + Send + Sync>;

/// The relay host connector. Cheap to clone; clones share one connector.
#[derive(Clone)]
pub struct RelayHost {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RelayHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayHost")
            .field("state", &self.status().state)
            .finish_non_exhaustive()
    }
}

impl Default for RelayHost {
    fn default() -> Self {
        Self::new(RelayHostConfig::default())
    }
}

struct Inner {
    config: RelayHostConfig,
    /// The `/v1` router relay streams are served with.
    service: Mutex<Option<Router>>,
    /// Serializes connect / disconnect / rotate / shutdown.
    control: tokio::sync::Mutex<Option<Session>>,
    shared: Mutex<Shared>,
    hook: Mutex<Option<SettingsHook>>,
    active: Arc<AtomicUsize>,
    /// Serving slots ([`RelayHostConfig::max_streams`]).
    limit: Arc<Semaphore>,
    /// Handshake slots ([`RelayHostConfig::max_handshakes`]).
    handshakes: Arc<Semaphore>,
    /// `sha256` of the current identity's open token; the control stream
    /// registers it and sends every change to the proxy.
    open_token: watch::Sender<[u8; 32]>,
    /// The open token hash the proxy confirmed on the current control
    /// stream (`None` while not registered).
    confirmed_token: watch::Sender<Option<[u8; 32]>>,
    /// Graceful end of every relay stream (server shutdown).
    graceful: CancellationToken,
    tasks: TaskTracker,
    stopped: AtomicBool,
}

/// The running control-stream loop.
struct Session {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

struct Shared {
    state: RelayState,
    settings: Option<RelaySettings>,
    address: Option<RelayAddress>,
    identity: Option<Arc<RelayIdentity>>,
    /// Whether `identity` is written to `config.identity_path`.
    persisted: bool,
    last_error: Option<String>,
    connected_since: Option<SystemTime>,
    binding: Option<BindingChoice>,
    /// Hard end of the current generation of relay streams (rotate,
    /// disconnect).
    streams: CancellationToken,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RelayHost {
    /// A disconnected connector.
    #[must_use]
    pub fn new(config: RelayHostConfig) -> Self {
        let limit = Arc::new(Semaphore::new(config.max_streams.max(1)));
        let handshakes = Arc::new(Semaphore::new(config.max_handshakes.max(1)));
        Self {
            inner: Arc::new(Inner {
                config,
                service: Mutex::new(None),
                control: tokio::sync::Mutex::new(None),
                shared: Mutex::new(Shared {
                    state: RelayState::Disconnected,
                    settings: None,
                    address: None,
                    identity: None,
                    persisted: false,
                    last_error: None,
                    connected_since: None,
                    binding: None,
                    streams: CancellationToken::new(),
                }),
                hook: Mutex::new(None),
                active: Arc::new(AtomicUsize::new(0)),
                limit,
                handshakes,
                open_token: watch::Sender::new([0; 32]),
                confirmed_token: watch::Sender::new(None),
                graceful: CancellationToken::new(),
                tasks: TaskTracker::new(),
                stopped: AtomicBool::new(false),
            }),
        }
    }

    /// Record settings changes (connect / disconnect) through `hook`.
    pub fn set_settings_hook(&self, hook: SettingsHook) {
        *lock(&self.inner.hook) = Some(hook);
    }

    /// Serve relay streams with `router` (the same `/v1` router as the TCP
    /// listener). Until this is called incoming streams are not accepted.
    /// [`RelayHost::shutdown`] releases it.
    pub fn set_service(&self, router: Router) {
        *lock(&self.inner.service) = Some(router);
    }

    /// The current settings, while connected (or reconnecting).
    #[must_use]
    pub fn settings(&self) -> Option<RelaySettings> {
        lock(&self.inner.shared).settings.clone()
    }

    /// Join the relay of `settings`, replacing any current connection, and
    /// return the link. The control stream runs in the background; see
    /// [`RelayHost::status`].
    ///
    /// # Errors
    /// [`RelayHostError::InvalidArgument`] for a bad URL or CA file,
    /// [`RelayHostError::Identity`] when the identity file cannot be loaded
    /// or created, [`RelayHostError::Stopping`] during shutdown.
    pub async fn connect(&self, settings: RelaySettings) -> Result<RelayLink, RelayHostError> {
        let inner = &self.inner;
        let mut control = inner.control.lock().await;
        if inner.stopped.load(Ordering::SeqCst) {
            return Err(RelayHostError::Stopping);
        }
        let address = settings.address()?;
        if let Some(ca) = &settings.extra_ca
            && !ca.is_file()
        {
            return Err(RelayHostError::InvalidArgument(format!(
                "the relay CA file {} does not exist",
                ca.display()
            )));
        }
        let client = RelayClient::new(
            address.clone(),
            ClientConfig {
                transport: settings.transport,
                extra_ca_pem: settings.extra_ca.clone(),
                heartbeat: inner.config.heartbeat,
                ..ClientConfig::default()
            },
        )
        .map_err(|error| RelayHostError::InvalidArgument(error.to_string()))?;
        let (identity, persisted) = self.identity_for(&settings)?;
        if let Some(previous) = control.take() {
            stop_session(previous).await;
        }
        let cancel = CancellationToken::new();
        {
            let mut shared = lock(&inner.shared);
            shared.streams.cancel();
            shared.streams = CancellationToken::new();
            shared.state = RelayState::Connecting;
            shared.settings = Some(settings.clone());
            shared.address = Some(address.clone());
            shared.identity = Some(identity.clone());
            shared.persisted = persisted;
            shared.last_error = None;
            shared.connected_since = None;
            shared.binding = None;
        }
        inner.open_token.send_replace(identity.open_token_hash());
        let task = tokio::spawn(host_loop(
            inner.clone(),
            client,
            identity.clone(),
            cancel.clone(),
        ));
        *control = Some(Session { cancel, task });
        drop(control);
        self.notify(Some(&settings));
        Ok(link_of(&address, &settings, &identity))
    }

    /// The identity a connection with `settings` uses, and whether it is
    /// persisted.
    fn identity_for(
        &self,
        settings: &RelaySettings,
    ) -> Result<(Arc<RelayIdentity>, bool), RelayHostError> {
        let fresh = || {
            RelayIdentity::generate()
                .map(Arc::new)
                .map_err(|error| RelayHostError::Identity(error.to_string()))
        };
        if settings.ephemeral {
            return Ok((fresh()?, false));
        }
        if let Some(path) = &self.inner.config.identity_path {
            let identity = RelayIdentity::load_or_create(path)
                .map_err(|error| RelayHostError::Identity(error.to_string()))?;
            return Ok((Arc::new(identity), true));
        }
        // An in-memory database: one throwaway identity for the process
        // lifetime, so a reconnect keeps the link.
        let current = {
            let shared = lock(&self.inner.shared);
            shared.identity.clone().filter(|_| !shared.persisted)
        };
        match current {
            Some(identity) => Ok((identity, false)),
            None => Ok((fresh()?, false)),
        }
    }

    /// Leave the relay: close the control stream (the proxy releases the
    /// room) and every open relay stream. Returns whether it was connected.
    pub async fn disconnect(&self) -> bool {
        let mut control = self.inner.control.lock().await;
        let Some(session) = control.take() else {
            return false;
        };
        stop_session(session).await;
        {
            let mut shared = lock(&self.inner.shared);
            shared.streams.cancel();
            shared.streams = CancellationToken::new();
            if shared.settings.as_ref().is_some_and(|s| s.ephemeral) {
                shared.identity = None;
            }
            shared.state = RelayState::Disconnected;
            shared.settings = None;
            shared.address = None;
            shared.last_error = None;
            shared.connected_since = None;
            shared.binding = None;
        }
        drop(control);
        self.notify(None);
        true
    }

    /// Issue a new pre-shared key: every open relay stream is closed, the
    /// proxy is told the new open token hash (so earlier links are refused
    /// before they reach this backend; waiting at most
    /// [`RelayHostConfig::register_timeout`] for its confirmation while
    /// connected), and earlier links would fail their handshake anyway.
    /// Returns the new link while connected.
    ///
    /// # Errors
    /// [`RelayHostError::FailedPrecondition`] when there is no identity (an
    /// in-memory database that never connected),
    /// [`RelayHostError::Identity`] when it cannot be saved.
    pub async fn rotate(&self) -> Result<Option<RelayLink>, RelayHostError> {
        let control = self.inner.control.lock().await;
        if self.inner.stopped.load(Ordering::SeqCst) {
            return Err(RelayHostError::Stopping);
        }
        let (current, persisted) = {
            let shared = lock(&self.inner.shared);
            (shared.identity.clone(), shared.persisted)
        };
        let (current, persisted) = match (current, &self.inner.config.identity_path) {
            (Some(identity), _) => (identity, persisted),
            (None, Some(path)) => (
                Arc::new(
                    RelayIdentity::load_or_create(path)
                        .map_err(|error| RelayHostError::Identity(error.to_string()))?,
                ),
                true,
            ),
            (None, None) => {
                return Err(RelayHostError::FailedPrecondition(
                    "there is no relay identity to rotate: connect to a relay first".to_owned(),
                ));
            }
        };
        let rotated = current
            .rotated()
            .map_err(|error| RelayHostError::Identity(error.to_string()))?;
        if persisted && let Some(path) = &self.inner.config.identity_path {
            rotated
                .save(path)
                .map_err(|error| RelayHostError::Identity(error.to_string()))?;
        }
        let rotated = Arc::new(rotated);
        let new_hash = rotated.open_token_hash();
        let (link, registered) = {
            let mut shared = lock(&self.inner.shared);
            shared.identity = Some(rotated.clone());
            shared.persisted = persisted;
            shared.streams.cancel();
            shared.streams = CancellationToken::new();
            let link = match (&shared.address, &shared.settings) {
                (Some(address), Some(settings)) => Some(link_of(address, settings, &rotated)),
                _ => None,
            };
            (link, shared.state == RelayState::Connected)
        };
        let mut confirmed = self.inner.confirmed_token.subscribe();
        self.inner.open_token.send_replace(new_hash);
        if registered {
            let confirmation = confirmed.wait_for(|hash| *hash == Some(new_hash));
            if tokio::time::timeout(self.inner.config.register_timeout, confirmation)
                .await
                .is_err()
            {
                tracing::warn!(
                    "relay host: the proxy did not confirm the new open token in time; \
                     old links still fail their handshake"
                );
            }
        }
        drop(control);
        Ok(link)
    }

    /// The full link while connected (or reconnecting). A secret.
    #[must_use]
    pub fn link(&self) -> Option<RelayLink> {
        let shared = lock(&self.inner.shared);
        match (&shared.address, &shared.settings, &shared.identity) {
            (Some(address), Some(settings), Some(identity)) => {
                Some(link_of(address, settings, identity))
            }
            _ => None,
        }
    }

    /// A snapshot of the connector.
    #[must_use]
    pub fn status(&self) -> RelayStatus {
        let shared = lock(&self.inner.shared);
        let room = shared.identity.as_ref().map(|identity| identity.room_id());
        let connected = shared.settings.is_some();
        RelayStatus {
            state: shared.state,
            proxy: shared.address.as_ref().map(RelayAddress::base_url),
            room_id: room
                .as_ref()
                .filter(|_| connected)
                .map(|room| room.as_str().to_owned()),
            redacted_link: match (&shared.address, &shared.settings, &shared.identity) {
                (Some(address), Some(settings), Some(identity)) => {
                    Some(link_of(address, settings, identity).redacted())
                }
                _ => None,
            },
            transport: shared.settings.as_ref().map(|settings| settings.transport),
            binding: shared.binding.clone(),
            last_error: shared.last_error.clone(),
            connected_since: shared.connected_since,
            active_streams: self.inner.active.load(Ordering::SeqCst),
            ephemeral: connected && !shared.persisted,
        }
    }

    /// Leave the relay for good (server shutdown): stop the control stream
    /// (the proxy releases the room), let open relay streams finish for at
    /// most [`RelayHostConfig::shutdown_grace`] — the live event streams
    /// already sent their `serverStopping` frame — then close the rest.
    /// Does not call the settings hook. Idempotent.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        let mut control = inner.control.lock().await;
        inner.stopped.store(true, Ordering::SeqCst);
        if let Some(session) = control.take() {
            stop_session(session).await;
        }
        inner.graceful.cancel();
        inner.tasks.close();
        let _ = tokio::time::timeout(inner.config.shutdown_grace, inner.tasks.wait()).await;
        let streams = {
            let mut shared = lock(&inner.shared);
            shared.state = RelayState::Disconnected;
            shared.connected_since = None;
            shared.streams.clone()
        };
        streams.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), inner.tasks.wait()).await;
        // Break the router <-> state reference cycle.
        *lock(&inner.service) = None;
    }

    fn notify(&self, settings: Option<&RelaySettings>) {
        let hook = lock(&self.inner.hook).clone();
        if let Some(hook) = hook {
            hook(settings);
        }
    }
}

fn link_of(
    address: &RelayAddress,
    settings: &RelaySettings,
    identity: &RelayIdentity,
) -> RelayLink {
    RelayLink::from_keys(
        address.clone(),
        identity.room_id(),
        settings.transport,
        *identity.noise().public(),
        identity.psk(),
    )
}

async fn stop_session(session: Session) {
    session.cancel.cancel();
    let mut task = session.task;
    if tokio::time::timeout(Duration::from_secs(3), &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
}

impl Inner {
    fn update(&self, change: impl FnOnce(&mut Shared)) {
        change(&mut lock(&self.shared));
    }
}

/// How one control-stream attempt ended.
struct Ended {
    kind: RetryKind,
    error: String,
}

/// Keep the room registered until `cancel`.
async fn host_loop(
    inner: Arc<Inner>,
    client: RelayClient,
    identity: Arc<RelayIdentity>,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new(inner.config.reconnect);
    loop {
        inner.update(|shared| shared.state = RelayState::Connecting);
        let ended = session(&inner, &client, &identity, &mut backoff, &cancel).await;
        inner.confirmed_token.send_replace(None);
        let ended = match ended {
            Ok(Some(ended)) => ended,
            Ok(None) => return,
            Err(error) => Ended {
                kind: RetryKind::of_client_error(&error),
                error: error.to_string(),
            },
        };
        if cancel.is_cancelled() {
            return;
        }
        let delay = backoff.next_delay(ended.kind);
        tracing::warn!(
            error = %ended.error,
            retry_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "relay host connection ended"
        );
        inner.update(|shared| {
            shared.state = RelayState::Backoff;
            shared.last_error = Some(ended.error);
            shared.connected_since = None;
        });
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// One control stream: register, then accept `incoming` streams until it
/// ends. `Ok(None)` when cancelled.
async fn session(
    inner: &Arc<Inner>,
    client: &RelayClient,
    identity: &Arc<RelayIdentity>,
    backoff: &mut Backoff,
    cancel: &CancellationToken,
) -> Result<Option<Ended>, ClientError> {
    let mut token_changes = inner.open_token.subscribe();
    let token_hash = *token_changes.borrow_and_update();
    let registered = async {
        let mut control = client.host().await?;
        let registration = register_host(
            &mut control,
            identity.signing_key(),
            &token_hash,
            inner.config.register_timeout,
        )
        .await?;
        Ok::<_, ClientError>((control, registration))
    };
    let (mut control, registration) = tokio::select! {
        () = cancel.cancelled() => return Ok(None),
        registered = registered => registered?,
    };
    let room = registration.room().clone();
    inner.confirmed_token.send_replace(Some(token_hash));
    // Hashes sent in `update_open_token`, oldest first, awaiting their ack.
    let mut unconfirmed = std::collections::VecDeque::new();
    backoff.connected();
    let binding = client.binding().await.ok();
    inner.update(|shared| {
        shared.state = RelayState::Connected;
        shared.last_error = None;
        shared.connected_since = Some(SystemTime::now());
        shared.binding = binding;
    });
    tracing::info!(room = room.as_str(), "relay host registered");
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                // Close our direction: the proxy releases the room.
                let _ = tokio::time::timeout(Duration::from_secs(2), control.close()).await;
                return Ok(None);
            }
            Ok(()) = token_changes.changed() => {
                let hash = *token_changes.borrow_and_update();
                let update = registration.update_open_token_frame(identity.signing_key(), &hash);
                if let Err(error) = control.send(update).await {
                    return Ok(Some(Ended {
                        kind: RetryKind::of_transport_error(&error),
                        error: error.to_string(),
                    }));
                }
                unconfirmed.push_back(hash);
            }
            item = control.next() => match item {
                Some(Ok(ProxyToHost { frame: Some(proxy_to_host::Frame::Incoming(incoming)) })) => {
                    spawn_stream(inner, client, &room, incoming.stream_id);
                }
                Some(Ok(ProxyToHost { frame: Some(proxy_to_host::Frame::OpenTokenUpdated(_)) })) => {
                    if let Some(hash) = unconfirmed.pop_front() {
                        inner.confirmed_token.send_replace(Some(hash));
                    }
                }
                Some(Ok(ProxyToHost { frame: Some(proxy_to_host::Frame::Error(error)) })) => {
                    let failure = TransportError::Status {
                        code: error.error_code(),
                        message: error.message,
                    };
                    return Ok(Some(Ended {
                        kind: RetryKind::of_transport_error(&failure),
                        error: failure.to_string(),
                    }));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    return Ok(Some(Ended {
                        kind: RetryKind::of_transport_error(&error),
                        error: error.to_string(),
                    }));
                }
                None => {
                    return Ok(Some(Ended {
                        kind: RetryKind::Normal,
                        error: "the relay ended the control stream".to_owned(),
                    }));
                }
            }
        }
    }
}

/// Accept one relay stream, run the Noise handshake, and serve HTTP on it.
///
/// The handshake runs on a handshake slot; the stream takes a serving slot
/// only once it is authenticated.
fn spawn_stream(inner: &Arc<Inner>, client: &RelayClient, room: &RoomId, stream_id: String) {
    let too_many_streams = || {
        tracing::warn!(
            max = inner.config.max_streams,
            "relay host: too many relay streams; not accepting another"
        );
    };
    if inner.limit.available_permits() == 0 {
        too_many_streams();
        return;
    }
    let Ok(handshake_slot) = inner.handshakes.clone().try_acquire_owned() else {
        tracing::warn!(
            max = inner.config.max_handshakes,
            "relay host: too many relay handshakes in progress; not accepting another"
        );
        return;
    };
    let Some(router) = lock(&inner.service).clone() else {
        tracing::warn!("relay host: no router to serve relay streams with");
        return;
    };
    let inner = inner.clone();
    let client = client.clone();
    let room = room.clone();
    let tasks = inner.tasks.clone();
    tasks.spawn(async move {
        let leg = match client.accept(&stream_id).await {
            Ok(leg) => leg,
            Err(error) => {
                tracing::warn!(%error, "relay host: accepting a relay stream failed");
                return;
            }
        };
        // The identity is read per stream: a rotation takes effect at the
        // next handshake, and closes streams of the old generation.
        let (identity, streams) = {
            let shared = lock(&inner.shared);
            (shared.identity.clone(), shared.streams.clone())
        };
        let Some(identity) = identity else { return };
        let handshake = NoiseStream::respond(
            leg,
            &room,
            identity.noise(),
            identity.psk(),
            TunnelConfig::default(),
        );
        let tunnel = match tokio::time::timeout(inner.config.handshake_timeout, handshake).await {
            Ok(Ok(tunnel)) => tunnel,
            Ok(Err(error)) => {
                // `TunnelError` messages never contain key material.
                tracing::warn!(%error, "relay host: handshake failed");
                return;
            }
            Err(_) => {
                tracing::warn!("relay host: handshake timed out");
                return;
            }
        };
        drop(handshake_slot);
        let Ok(permit) = inner.limit.clone().try_acquire_owned() else {
            tracing::warn!(
                max = inner.config.max_streams,
                "relay host: too many relay streams; closing an authenticated one"
            );
            return;
        };
        let io = conn::Killable::new(
            tunnel,
            &streams,
            conn::ActiveGuard::new(&inner.active),
            permit,
        );
        conn::serve(
            io,
            router,
            inner.graceful.clone(),
            streams,
            inner.config.shutdown_grace,
        )
        .await;
    });
}
