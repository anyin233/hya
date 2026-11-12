//! `hya proxy`: a thin CLI around [`hya_relay::server::RelayServer`] (the
//! relay proxy, docs/relay.md). Dispatched before any runtime composition —
//! like `hya update` — because it needs no config, providers, or database.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use hya_relay::proxy::{
    DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_ROOMS, DEFAULT_MAX_STREAMS_PER_ROOM,
    DEFAULT_STREAM_RATE_BYTES_PER_SEC, ProxyLimits,
};
use hya_relay::server::{
    DEFAULT_DRAIN_TIMEOUT, ForwardedHeader, RelayServer, RelayServerConfig, TlsFiles,
};

/// Default `hya proxy` bind host.
pub(crate) const DEFAULT_PROXY_HOST: &str = "0.0.0.0";
/// Default `hya proxy` bind port.
pub(crate) const DEFAULT_PROXY_PORT: u16 = 8766;

/// `--tls-cert`/`--tls-key`, `--path-prefix`, `--trust-forwarded <header>`, and
/// the four [`ProxyLimits`] knobs an operator can meaningfully choose; every
/// other limit keeps its library default (durations in whole seconds since
/// nothing in the codebase parses `10s`-style durations yet).
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
    /// Identify clients by the address in this one forwarding header
    /// instead of the socket address: `cf-connecting-ip`, `x-real-ip`, or
    /// `x-forwarded-for` (its rightmost entry, the one the trusted hop
    /// appended). Only safe when every client reaches the proxy through a
    /// hop that sets exactly that header; other headers are ignored.
    #[arg(long, value_name = "HEADER", value_parser = parse_forwarded_header)]
    pub(crate) trust_forwarded: Option<ForwardedHeader>,
    /// Maximum registered rooms (`RESOURCE_EXHAUSTED` over the limit).
    #[arg(long, default_value_t = DEFAULT_MAX_ROOMS)]
    pub(crate) max_rooms: usize,
    /// Maximum concurrent streams per room.
    #[arg(long, default_value_t = DEFAULT_MAX_STREAMS_PER_ROOM)]
    pub(crate) max_streams_per_room: usize,
    /// A stream leg or control stream idle this long (seconds; heartbeats
    /// count as activity) is closed with `DEADLINE_EXCEEDED`.
    #[arg(long, default_value_t = DEFAULT_IDLE_TIMEOUT.as_secs())]
    pub(crate) idle_timeout_secs: u64,
    /// Byte-rate cap per stream and direction; `0` disables it.
    #[arg(long, default_value_t = DEFAULT_STREAM_RATE_BYTES_PER_SEC)]
    pub(crate) stream_rate_bytes_per_sec: u64,
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
            idle_timeout: Duration::from_secs(self.idle_timeout_secs),
            stream_rate_bytes_per_sec: self.stream_rate_bytes_per_sec,
            ..ProxyLimits::default()
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

fn parse_forwarded_header(text: &str) -> Result<ForwardedHeader, String> {
    text.parse()
        .map_err(|error: hya_relay::server::UnknownForwardedHeader| error.to_string())
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
        assert!(cli.proxy.trust_forwarded.is_none());
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
    fn overrides_the_four_limit_flags_and_defaults_the_rest() {
        let cli = parse(&[
            "--max-rooms",
            "1",
            "--max-streams-per-room",
            "2",
            "--idle-timeout-secs",
            "6",
            "--stream-rate-bytes-per-sec",
            "7",
        ])
        .expect("every limit flag parses");
        let limits = cli.proxy.limits();
        assert_eq!(limits.max_rooms, 1);
        assert_eq!(limits.max_streams_per_room, 2);
        assert_eq!(limits.idle_timeout, std::time::Duration::from_secs(6));
        assert_eq!(limits.stream_rate_bytes_per_sec, 7);
        // Everything else keeps the library policy.
        assert_eq!(
            limits,
            hya_relay::proxy::ProxyLimits {
                max_rooms: 1,
                max_streams_per_room: 2,
                idle_timeout: std::time::Duration::from_secs(6),
                stream_rate_bytes_per_sec: 7,
                ..hya_relay::proxy::ProxyLimits::default()
            }
        );
    }

    #[test]
    fn retired_limit_flags_are_rejected() {
        for flag in [
            "--max-streams-per-peer",
            "--max-streams",
            "--max-rooms-per-peer",
            "--max-pending-registrations-per-peer",
            "--max-pending-registrations",
            "--stream-rate-burst-bytes",
            "--max-chunk-data",
            "--early-data-limit",
            "--max-early-data-bytes",
            "--accept-timeout-secs",
            "--handshake-timeout-secs",
        ] {
            assert!(parse(&[flag, "1"]).is_err(), "{flag} must be gone");
        }
    }

    #[test]
    fn trust_forwarded_names_exactly_one_header() {
        use hya_relay::server::ForwardedHeader;
        assert!(
            parse(&["--trust-forwarded"]).is_err(),
            "a header is required"
        );
        assert!(parse(&["--trust-forwarded", "forwarded"]).is_err());
        for (name, header) in [
            ("cf-connecting-ip", ForwardedHeader::CfConnectingIp),
            ("x-real-ip", ForwardedHeader::XRealIp),
            ("x-forwarded-for", ForwardedHeader::XForwardedFor),
        ] {
            let cli = parse(&["--trust-forwarded", name]).expect("parses");
            assert_eq!(cli.proxy.trust_forwarded, Some(header));
        }
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
            "x-real-ip",
        ])
        .expect("parses");
        assert_eq!(cli.proxy.host, "127.0.0.1");
        assert_eq!(cli.proxy.port, 0);
        assert_eq!(cli.proxy.path_prefix.as_deref(), Some("/relay"));
        assert_eq!(
            cli.proxy.trust_forwarded,
            Some(hya_relay::server::ForwardedHeader::XRealIp)
        );
    }
}
