//! `hya proxy`: a thin CLI around [`hya_relay::server::RelayServer`] (the
//! relay proxy, docs/relay.md). Dispatched before any runtime composition —
//! like `hya update` — because it needs no config, providers, or database.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use hya_relay::proxy::{
    DEFAULT_ACCEPT_TIMEOUT, DEFAULT_EARLY_DATA_LIMIT, DEFAULT_HANDSHAKE_TIMEOUT,
    DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_CHUNK_DATA, DEFAULT_MAX_PENDING_REGISTRATIONS_PER_PEER,
    DEFAULT_MAX_ROOMS, DEFAULT_MAX_ROOMS_PER_PEER, DEFAULT_MAX_STREAMS_PER_PEER,
    DEFAULT_MAX_STREAMS_PER_ROOM, DEFAULT_STREAM_RATE_BURST_BYTES,
    DEFAULT_STREAM_RATE_BYTES_PER_SEC, ProxyLimits,
};
use hya_relay::server::{DEFAULT_DRAIN_TIMEOUT, RelayServer, RelayServerConfig, TlsFiles};

/// Default `hya proxy` bind host.
pub(crate) const DEFAULT_PROXY_HOST: &str = "0.0.0.0";
/// Default `hya proxy` bind port.
pub(crate) const DEFAULT_PROXY_PORT: u16 = 8766;

/// `--tls-cert`/`--tls-key`, `--path-prefix`, `--trust-forwarded`, and every
/// [`ProxyLimits`] field, one flag each (kebab-case; durations in whole
/// seconds since nothing in the codebase parses `10s`-style durations yet).
#[derive(clap::Args, Debug, Clone)]
pub(crate) struct ProxyArgs {
    /// Bind host. The proxy has no local trust boundary (Noise end-to-end
    /// encrypts the payload), so the default is every interface.
    #[arg(long, default_value_t = DEFAULT_PROXY_HOST.to_string())]
    pub(crate) host: String,
    /// Bind port.
    #[arg(long, default_value_t = DEFAULT_PROXY_PORT)]
    pub(crate) port: u16,
    /// TLS certificate chain (PEM, leaf first). Requires `--tls-key`.
    #[arg(long, requires = "tls_key")]
    pub(crate) tls_cert: Option<PathBuf>,
    /// TLS private key (PEM: PKCS#8, PKCS#1, or SEC1). Requires `--tls-cert`.
    #[arg(long, requires = "tls_cert")]
    pub(crate) tls_key: Option<PathBuf>,
    /// Serve both bindings under a path prefix (for hops that route by path
    /// and cannot rewrite gRPC paths).
    #[arg(long)]
    pub(crate) path_prefix: Option<String>,
    /// Identify clients by `CF-Connecting-IP` / `X-Real-IP` /
    /// `X-Forwarded-For` instead of the socket address. Only safe behind a
    /// hop that overwrites these headers.
    #[arg(long)]
    pub(crate) trust_forwarded: bool,
    /// Maximum registered rooms (`RESOURCE_EXHAUSTED` over the limit).
    #[arg(long, default_value_t = DEFAULT_MAX_ROOMS)]
    pub(crate) max_rooms: usize,
    /// Maximum concurrent streams per room.
    #[arg(long, default_value_t = DEFAULT_MAX_STREAMS_PER_ROOM)]
    pub(crate) max_streams_per_room: usize,
    /// Maximum concurrent streams opened by one client identity.
    #[arg(long, default_value_t = DEFAULT_MAX_STREAMS_PER_PEER)]
    pub(crate) max_streams_per_peer: usize,
    /// Maximum rooms registered by one client identity.
    #[arg(long, default_value_t = DEFAULT_MAX_ROOMS_PER_PEER)]
    pub(crate) max_rooms_per_peer: usize,
    /// Maximum unfinished host registrations per client identity.
    #[arg(long, default_value_t = DEFAULT_MAX_PENDING_REGISTRATIONS_PER_PEER)]
    pub(crate) max_pending_registrations_per_peer: usize,
    /// A stream leg or control stream idle this long (seconds; heartbeats
    /// count as activity) is closed with `DEADLINE_EXCEEDED`.
    #[arg(long, default_value_t = DEFAULT_IDLE_TIMEOUT.as_secs())]
    pub(crate) idle_timeout_secs: u64,
    /// Byte-rate cap per stream and direction; `0` disables it.
    #[arg(long, default_value_t = DEFAULT_STREAM_RATE_BYTES_PER_SEC)]
    pub(crate) stream_rate_bytes_per_sec: u64,
    /// Token-bucket burst for `--stream-rate-bytes-per-sec`.
    #[arg(long, default_value_t = DEFAULT_STREAM_RATE_BURST_BYTES)]
    pub(crate) stream_rate_burst_bytes: u64,
    /// Largest accepted `data` payload; a larger chunk fails the stream.
    #[arg(long, default_value_t = DEFAULT_MAX_CHUNK_DATA)]
    pub(crate) max_chunk_data: usize,
    /// Opener `data` bytes buffered before the host accepts the stream.
    #[arg(long, default_value_t = DEFAULT_EARLY_DATA_LIMIT)]
    pub(crate) early_data_limit: usize,
    /// Seconds an `Open` waits for the host's `Accept` before `UNAVAILABLE`.
    #[arg(long, default_value_t = DEFAULT_ACCEPT_TIMEOUT.as_secs())]
    pub(crate) accept_timeout_secs: u64,
    /// Seconds allowed for the first frame of every stream (registration,
    /// `open`, `accept`) before `DEADLINE_EXCEEDED`.
    #[arg(long, default_value_t = DEFAULT_HANDSHAKE_TIMEOUT.as_secs())]
    pub(crate) handshake_timeout_secs: u64,
    /// Seconds shutdown waits for streams and connections to drain before
    /// cutting them.
    #[arg(long, default_value_t = DEFAULT_DRAIN_TIMEOUT.as_secs())]
    pub(crate) drain_timeout_secs: u64,
}

impl ProxyArgs {
    fn limits(&self) -> ProxyLimits {
        ProxyLimits {
            max_rooms: self.max_rooms,
            max_streams_per_room: self.max_streams_per_room,
            max_streams_per_peer: self.max_streams_per_peer,
            max_rooms_per_peer: self.max_rooms_per_peer,
            max_pending_registrations_per_peer: self.max_pending_registrations_per_peer,
            idle_timeout: Duration::from_secs(self.idle_timeout_secs),
            stream_rate_bytes_per_sec: self.stream_rate_bytes_per_sec,
            stream_rate_burst_bytes: self.stream_rate_burst_bytes,
            max_chunk_data: self.max_chunk_data,
            early_data_limit: self.early_data_limit,
            accept_timeout: Duration::from_secs(self.accept_timeout_secs),
            handshake_timeout: Duration::from_secs(self.handshake_timeout_secs),
        }
    }

    fn tls(&self) -> Option<TlsFiles> {
        match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) => Some(TlsFiles {
                cert: cert.clone(),
                key: key.clone(),
            }),
            _ => None,
        }
    }
}

/// Run `hya proxy` until SIGINT/SIGTERM, then drain and exit 0.
pub(crate) async fn cmd_proxy(args: ProxyArgs) -> anyhow::Result<()> {
    let bind: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", args.host, args.port))?;
    let secure = args.tls().is_some();
    let mut config = RelayServerConfig::new(bind)
        .trust_forwarded(args.trust_forwarded)
        .limits(args.limits())
        .drain_timeout(Duration::from_secs(args.drain_timeout_secs));
    if let Some(prefix) = &args.path_prefix {
        config = config
            .path_prefix(prefix)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    }
    if let Some(tls) = args.tls() {
        config = config.tls(tls);
    }
    let prefix = config.prefix().to_owned();
    let (addr, serve) = RelayServer::bind(config, shutdown_signal())
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let scheme = if secure { "https" } else { "http" };
    println!("hya proxy listening on {scheme}://{addr}{prefix}");
    serve.await;
    Ok(())
}

/// Resolve once SIGINT or SIGTERM fires.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let ctrl_c = tokio::signal::ctrl_c();
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(signal) => signal,
        Err(error) => {
            eprintln!("hya proxy: failed to install SIGTERM handler: {error}");
            let _ = ctrl_c.await;
            return;
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate.recv() => {}
    }
    eprintln!("hya proxy: shutting down — draining streams");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        proxy: super::ProxyArgs,
    }

    fn parse(args: &[&str]) -> Result<TestCli, clap::Error> {
        let mut full = vec!["hya-proxy-test"];
        full.extend_from_slice(args);
        TestCli::try_parse_from(full)
    }

    #[test]
    fn defaults_match_the_library_and_8766() {
        let cli = parse(&[]).expect("no flags parses");
        assert_eq!(cli.proxy.host, super::DEFAULT_PROXY_HOST);
        assert_eq!(cli.proxy.port, super::DEFAULT_PROXY_PORT);
        assert_eq!(cli.proxy.port, 8766);
        assert_eq!(cli.proxy.host, "0.0.0.0");
        assert!(!cli.proxy.trust_forwarded);
        assert!(cli.proxy.tls_cert.is_none());
        assert!(cli.proxy.tls_key.is_none());
        let limits = cli.proxy.limits();
        assert_eq!(limits, hya_relay::proxy::ProxyLimits::default());
        assert_eq!(
            cli.proxy.drain_timeout_secs,
            hya_relay::server::DEFAULT_DRAIN_TIMEOUT.as_secs()
        );
    }

    #[test]
    fn tls_cert_and_key_must_be_given_together() {
        assert!(parse(&["--tls-cert", "c.pem"]).is_err());
        assert!(parse(&["--tls-key", "k.pem"]).is_err());
        let cli = parse(&["--tls-cert", "c.pem", "--tls-key", "k.pem"]).expect("both given");
        let tls = cli.proxy.tls().expect("tls files built");
        assert_eq!(tls.cert.to_string_lossy(), "c.pem");
        assert_eq!(tls.key.to_string_lossy(), "k.pem");
    }

    #[test]
    fn overrides_every_limit_flag() {
        let cli = parse(&[
            "--max-rooms",
            "1",
            "--max-streams-per-room",
            "2",
            "--max-streams-per-peer",
            "3",
            "--max-rooms-per-peer",
            "4",
            "--max-pending-registrations-per-peer",
            "5",
            "--idle-timeout-secs",
            "6",
            "--stream-rate-bytes-per-sec",
            "7",
            "--stream-rate-burst-bytes",
            "8",
            "--max-chunk-data",
            "9",
            "--early-data-limit",
            "10",
            "--accept-timeout-secs",
            "11",
            "--handshake-timeout-secs",
            "12",
        ])
        .expect("all limit flags parse");
        let limits = cli.proxy.limits();
        assert_eq!(limits.max_rooms, 1);
        assert_eq!(limits.max_streams_per_room, 2);
        assert_eq!(limits.max_streams_per_peer, 3);
        assert_eq!(limits.max_rooms_per_peer, 4);
        assert_eq!(limits.max_pending_registrations_per_peer, 5);
        assert_eq!(limits.idle_timeout, std::time::Duration::from_secs(6));
        assert_eq!(limits.stream_rate_bytes_per_sec, 7);
        assert_eq!(limits.stream_rate_burst_bytes, 8);
        assert_eq!(limits.max_chunk_data, 9);
        assert_eq!(limits.early_data_limit, 10);
        assert_eq!(limits.accept_timeout, std::time::Duration::from_secs(11));
        assert_eq!(limits.handshake_timeout, std::time::Duration::from_secs(12));
    }

    #[test]
    fn host_port_and_path_prefix_override() {
        let cli = parse(&[
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--path-prefix",
            "/relay",
            "--trust-forwarded",
        ])
        .expect("parses");
        assert_eq!(cli.proxy.host, "127.0.0.1");
        assert_eq!(cli.proxy.port, 0);
        assert_eq!(cli.proxy.path_prefix.as_deref(), Some("/relay"));
        assert!(cli.proxy.trust_forwarded);
    }
}
