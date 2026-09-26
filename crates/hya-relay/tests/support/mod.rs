//! Shared test support: a relay server runner, TLS material, in-process
//! intermediaries ("hops"), and a Noise echo backend driven through the
//! relay client.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod hops;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use futures::StreamExt;
use hya_relay::client::{
    Backoff, ClientError, ReconnectPolicy, RelayClient, RetryKind, register_host,
};
use hya_relay::keys::{Psk, StaticKeypair};
use hya_relay::link::{RelayAddress, RelayLink, RoomId, Transport};
use hya_relay::proto::{ProxyToHost, proxy_to_host};
use hya_relay::server::{RelayServer, RelayServerConfig, TlsFiles};
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Upper bound for anything a test waits on.
pub const WAIT: Duration = Duration::from_secs(10);

// ---- relay server ----

pub fn relay_config() -> RelayServerConfig {
    RelayServerConfig::new("127.0.0.1:0".parse().unwrap())
}

pub struct Relay {
    pub addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: JoinHandle<()>,
}

impl Relay {
    pub async fn start(config: RelayServerConfig) -> Self {
        let (stop, stopped) = oneshot::channel::<()>();
        let (addr, serve) = RelayServer::bind(config, async move {
            let _ = stopped.await;
        })
        .await
        .unwrap();
        Self {
            addr,
            stop: Some(stop),
            done: tokio::spawn(serve),
        }
    }

    /// Plaintext address of the relay itself (no prefix).
    pub fn address(&self) -> RelayAddress {
        plain(self.addr, "")
    }

    pub async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let _ = timeout(WAIT, self.done).await;
    }
}

/// `hya+insecure://127.0.0.1:<port><prefix>`.
pub fn plain(addr: SocketAddr, prefix: &str) -> RelayAddress {
    RelayAddress::new(false, "127.0.0.1", Some(addr.port()), prefix).unwrap()
}

// ---- TLS ----

/// A self-signed certificate for `localhost` in a scratch directory; the
/// certificate file doubles as the client's extra CA.
pub struct TestTls {
    pub files: TlsFiles,
    pub ca: PathBuf,
}

impl TestTls {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "hya-relay-client-tls-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        std::fs::write(&cert, certified.cert.pem()).unwrap();
        std::fs::write(&key, certified.key_pair.serialize_pem()).unwrap();
        Self {
            files: TlsFiles {
                cert: cert.clone(),
                key,
            },
            ca: cert,
        }
    }
}

impl Drop for TestTls {
    fn drop(&mut self) {
        if let Some(dir) = self.ca.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---- a Noise echo backend behind the host control stream ----

/// The backend's identity: room key, Noise static key, PSK.
pub struct Identity {
    pub key: SigningKey,
    pub noise: StaticKeypair,
    pub psk: Psk,
}

impl Identity {
    pub fn new(seed: u8) -> Self {
        Self {
            key: SigningKey::from_bytes(&[seed; 32]),
            noise: StaticKeypair::from_secret([seed.wrapping_add(100); 32]).unwrap(),
            psk: Psk::from_bytes([seed.wrapping_add(200); 32]),
        }
    }

    pub fn room(&self) -> RoomId {
        RoomId::from_ed25519(self.key.verifying_key().as_bytes())
    }

    /// The relay link a client would be given.
    pub fn link(&self, address: RelayAddress, transport: Transport) -> RelayLink {
        RelayLink::new(
            address,
            self.room(),
            transport,
            *self.noise.public(),
            *self.psk.as_bytes(),
        )
    }
}

/// A backend kept online by a reconnect loop: it registers, accepts every
/// incoming stream as a Noise responder, and echoes bytes back until EOF.
pub struct Backend {
    pub identity: Arc<Identity>,
    /// Successful registrations so far (1 after the first).
    pub registrations: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Backend {
    /// Start the loop and wait for the first registration.
    pub async fn start(client: RelayClient, identity: Identity, policy: ReconnectPolicy) -> Self {
        let identity = Arc::new(identity);
        let registrations = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn(host_loop(
            client,
            identity.clone(),
            registrations.clone(),
            policy,
        ));
        let backend = Self {
            identity,
            registrations,
            task,
        };
        backend.wait_registrations(1).await;
        backend
    }

    pub async fn wait_registrations(&self, count: usize) {
        timeout(WAIT, async {
            while self.registrations.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the backend registered in time");
    }
}

/// Fast reconnects for tests.
pub fn quick_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(200),
        conflict_initial: Duration::from_millis(50),
        conflict_max: Duration::from_millis(500),
        stable_after: Duration::from_secs(1),
    }
}

async fn host_loop(
    client: RelayClient,
    identity: Arc<Identity>,
    registrations: Arc<AtomicUsize>,
    policy: ReconnectPolicy,
) {
    let mut backoff = Backoff::new(policy);
    loop {
        let kind = match session(&client, &identity, &registrations, &mut backoff).await {
            Ok(kind) => kind,
            Err(error) => RetryKind::of_client_error(&error),
        };
        tokio::time::sleep(backoff.next_delay(kind)).await;
    }
}

/// One control stream: register, then serve `incoming` until it ends.
async fn session(
    client: &RelayClient,
    identity: &Arc<Identity>,
    registrations: &AtomicUsize,
    backoff: &mut Backoff,
) -> Result<RetryKind, ClientError> {
    let mut control = client.host().await?;
    let room = register_host(&mut control, &identity.key, WAIT).await?;
    registrations.fetch_add(1, Ordering::SeqCst);
    backoff.connected();
    while let Some(item) = control.next().await {
        match item {
            Ok(ProxyToHost {
                frame: Some(proxy_to_host::Frame::Incoming(incoming)),
            }) => {
                let client = client.clone();
                let identity = identity.clone();
                let room = room.clone();
                tokio::spawn(async move {
                    let Ok(leg) = client.accept(&incoming.stream_id).await else {
                        return;
                    };
                    let Ok(tunnel) = timeout(
                        WAIT,
                        NoiseStream::respond(
                            leg,
                            &room,
                            &identity.noise,
                            &identity.psk,
                            TunnelConfig::default(),
                        ),
                    )
                    .await
                    else {
                        return;
                    };
                    if let Ok(tunnel) = tunnel {
                        echo(tunnel).await;
                    }
                });
            }
            Ok(_) => {}
            Err(error) => return Ok(RetryKind::of_transport_error(&error)),
        }
    }
    Ok(RetryKind::Normal)
}

async fn echo<T>(mut tunnel: T)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match tunnel.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tunnel.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    }
    let _ = tunnel.shutdown().await;
}

/// A client-side tunnel to the backend of `link`.
pub async fn connect(
    client: &RelayClient,
    link: &RelayLink,
) -> Result<NoiseStream<hya_relay::transport::ChunkTransport>, ClientError> {
    let leg = client.open(link.room_id()).await?;
    timeout(
        WAIT,
        NoiseStream::initiate_link(leg, link, TunnelConfig::default()),
    )
    .await
    .map_err(|_| ClientError::Timeout("noise handshake".into()))?
    .map_err(|error| ClientError::Transport(error.to_string()))
}

/// Send `payload` through a fresh tunnel and check the echo.
pub async fn round_trip(client: &RelayClient, link: &RelayLink, payload: &[u8]) {
    let mut tunnel = connect(client, link).await.expect("tunnel opened");
    timeout(WAIT, async {
        tunnel.write_all(payload).await.unwrap();
        tunnel.shutdown().await.unwrap();
        let mut echoed = Vec::new();
        tunnel.read_to_end(&mut echoed).await.unwrap();
        assert_eq!(echoed, payload);
    })
    .await
    .expect("round trip in time");
}

/// Send `message` on an open tunnel and read the same number of bytes back.
pub async fn ping<T>(tunnel: &mut T, message: &[u8]) -> std::io::Result<()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tunnel.write_all(message).await?;
    tunnel.flush().await?;
    let mut back = vec![0u8; message.len()];
    tunnel.read_exact(&mut back).await?;
    assert_eq!(back, message);
    Ok(())
}
