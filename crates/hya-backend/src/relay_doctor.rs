//! `hya relay doctor <proxy-url|link|->`: probes a relay path end to end and
//! recommends a `t=` value (docs/relay.md "Diagnostics", ADR-0025 D9).
//!
//! The probes only need the relay address, so a proxy URL or a redacted link
//! (`hya://host/<room>`, no `#…`) is enough. A full link is accepted too — on
//! stdin (`-`) quietly, as an argument with a warning, since arguments are
//! visible in process listings; `--measure-idle` needs it (opening a stream
//! to the room takes the link's open token).
//!
//! Dispatched before any runtime composition, like `hya proxy` and
//! `hya update`: it only opens network connections, never a config, a
//! database, or providers.

use std::path::PathBuf;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use hya_relay::client::{Binding, ClientConfig, ProbeFailure, ProbeFailureKind, RelayClient};
use hya_relay::keys::OpenToken;
use hya_relay::link::{RelayAddress, RelayLink, RoomId, redact_input};
use hya_relay::proto::{Chunk, Heartbeat, chunk};
use hya_relay::transport::TransportError;
use serde::Serialize;
use tokio::time::timeout;

/// `hya relay` subcommands.
#[derive(clap::Subcommand)]
pub(crate) enum RelayCommand {
    /// Probe a proxy URL or relay link and recommend a `t=` value.
    Doctor {
        /// `https://…`/`http://…` proxy URL, a redacted
        /// `hya://`/`hya+insecure://` link (no `#…`), a full link, or `-` to
        /// read it from stdin. The link's secret is never printed; a full
        /// link as an argument prints a process-listing warning.
        target: String,
        /// Extra trusted CA certificates (PEM), for a private CA.
        #[arg(long)]
        relay_ca: Option<PathBuf>,
        /// Deadline for each probe, in seconds.
        #[arg(long, default_value_t = 5)]
        timeout: u64,
        /// Also measure how long an idle stream survives on this path
        /// (bounded at 130 s); needs a link with a room that currently has a
        /// host registered.
        #[arg(long)]
        measure_idle: bool,
        /// Emit the report as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// Run `hya relay doctor`. Returns the process exit code (0: at least one
/// binding works; 1: neither does).
pub(crate) async fn run(command: RelayCommand) -> anyhow::Result<i32> {
    let RelayCommand::Doctor {
        target,
        relay_ca,
        timeout: timeout_secs,
        measure_idle,
        json,
    } = command;
    let options = DoctorOptions {
        relay_ca,
        timeout: Duration::from_secs(timeout_secs),
        measure_idle,
        idle_cap: MAX_IDLE_MEASUREMENT,
    };
    let target = resolve_target_from(&target, std::io::stdin().lock(), &mut std::io::stderr())?;
    let report = diagnose(&target, options).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print!("{}", format_report(&report));
    }
    Ok(if report.reachable { 0 } else { 1 })
}

/// Printed when a full link (with its `#` secret) is a command-line argument.
pub(crate) const ARGV_WARNING: &str = "the relay link was given as an argument, so its secret is visible in process listings; `hya relay doctor` only needs the redacted link (drop the `#…` part) or the proxy URL, or pass `-` and write the link to stdin";

/// Whether `target` is a link carrying its secret fragment.
fn carries_secret(target: &str) -> bool {
    target.contains('#')
}

/// Resolve the target from argv or stdin (`-`), emitting the argv warning when
/// a secret-bearing link is passed directly on the command line.
fn resolve_target_from(
    target: &str,
    mut stdin: impl std::io::BufRead,
    mut stderr: impl std::io::Write,
) -> anyhow::Result<String> {
    if target == "-" {
        let mut line = String::new();
        stdin
            .read_line(&mut line)
            .map_err(|error| anyhow::anyhow!("reading the target from stdin: {error}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            anyhow::bail!("no relay link or proxy URL on stdin");
        }
        Ok(trimmed.to_owned())
    } else {
        if carries_secret(target) {
            let _ = writeln!(stderr, "warning: {ARGV_WARNING}");
        }
        Ok(target.to_owned())
    }
}

/// Upper bound of `--measure-idle` (ADR-0025 D9).
pub(crate) const MAX_IDLE_MEASUREMENT: Duration = Duration::from_secs(130);

/// Options for [`diagnose`].
#[derive(Debug, Clone)]
pub(crate) struct DoctorOptions {
    pub(crate) relay_ca: Option<PathBuf>,
    pub(crate) timeout: Duration,
    pub(crate) measure_idle: bool,
    pub(crate) idle_cap: Duration,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            relay_ca: None,
            timeout: Duration::from_secs(5),
            measure_idle: false,
            idle_cap: MAX_IDLE_MEASUREMENT,
        }
    }
}

/// One binding's probe result.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeReport {
    pub(crate) ok: bool,
    pub(crate) kind: Option<String>,
    pub(crate) detail: Option<String>,
}

impl ProbeReport {
    fn ok() -> Self {
        Self {
            ok: true,
            kind: None,
            detail: None,
        }
    }

    fn failed(failure: &ProbeFailure) -> Self {
        Self {
            ok: false,
            kind: Some(format!("{:?}", failure.kind)),
            detail: Some(failure.detail.clone()),
        }
    }
}

/// Result of `--measure-idle`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdleReport {
    /// Whether the stream was cut before `idle_cap`.
    pub(crate) cut: bool,
    /// How long it survived (the cap, if it was never cut).
    pub(crate) after_secs: u64,
}

/// The full `hya relay doctor` report.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DoctorReport {
    /// The target, redacted (a link's secret is never printed).
    pub(crate) target: String,
    pub(crate) secure: bool,
    /// Whether at least one binding works.
    pub(crate) reachable: bool,
    pub(crate) grpc: ProbeReport,
    pub(crate) ws: ProbeReport,
    /// `false` only when both probes report `WrongPath`.
    pub(crate) prefix_ok: bool,
    /// `auto`, `grpc`, or `ws`; absent when neither binding works.
    pub(crate) recommended_transport: Option<String>,
    pub(crate) advice: Vec<String>,
    pub(crate) idle: Option<IdleReport>,
}

/// Probe `target` (a proxy URL or a relay link) and build the report.
pub(crate) async fn diagnose(target: &str, options: DoctorOptions) -> anyhow::Result<DoctorReport> {
    let Target {
        address,
        room,
        token,
        shown: redacted,
    } = parse_target(target)?;
    let config = ClientConfig {
        extra_ca_pem: options.relay_ca.clone(),
        connect_timeout: options.timeout,
        probe_timeout: options.timeout,
        ..ClientConfig::default()
    };
    let client = RelayClient::new(address.clone(), config)
        .map_err(|error| anyhow::anyhow!("relay client configuration: {error}"))?;
    let grpc = client.probe(Binding::Grpc).await;
    let ws = client.probe(Binding::Ws).await;
    let grpc_ok = grpc.is_ok();
    let ws_ok = ws.is_ok();
    let mut advice = Vec::new();
    if let Err(failure) = &grpc {
        advice.push(format!("gRPC: {}", advice_for(failure.kind)));
    }
    if let Err(failure) = &ws {
        advice.push(format!("WebSocket: {}", advice_for(failure.kind)));
    }
    let prefix_ok = !matches!(
        (&grpc, &ws),
        (Err(g), Err(w)) if g.kind == ProbeFailureKind::WrongPath && w.kind == ProbeFailureKind::WrongPath
    );
    let recommended_transport = match (grpc_ok, ws_ok) {
        (true, _) => Some("auto".to_owned()),
        (false, true) => Some("ws".to_owned()),
        (false, false) => None,
    };
    let idle = if options.measure_idle {
        Some(measure_idle(&client, room.as_ref(), token.as_ref(), options.idle_cap).await?)
    } else {
        None
    };
    Ok(DoctorReport {
        target: redacted,
        secure: address.is_secure(),
        reachable: grpc_ok || ws_ok,
        grpc: grpc.map_or_else(
            |failure| ProbeReport::failed(&failure),
            |()| ProbeReport::ok(),
        ),
        ws: ws.map_or_else(
            |failure| ProbeReport::failed(&failure),
            |()| ProbeReport::ok(),
        ),
        prefix_ok,
        recommended_transport,
        advice,
        idle,
    })
}

/// One-line advice per [`ProbeFailureKind`] (ADR-0025 D9 diagnostics).
fn advice_for(kind: ProbeFailureKind) -> &'static str {
    match kind {
        ProbeFailureKind::NoHttp2
        | ProbeFailureKind::TrailersStripped
        | ProbeFailureKind::HopRejected
        | ProbeFailureKind::Timeout => {
            "this hop does not carry gRPC end to end; pin or let auto pick t=ws"
        }
        ProbeFailureKind::WrongPath => "the path prefix does not match the proxy's --path-prefix",
        ProbeFailureKind::Tls => "check --relay-ca (private CA) or the host name",
        ProbeFailureKind::Connect => "cannot reach the host/port; check the address and firewall",
        ProbeFailureKind::Unexpected => "unexpected answer; see the detail",
    }
}

/// A parsed doctor target.
struct Target {
    address: RelayAddress,
    /// The room, from a (full or redacted) link.
    room: Option<RoomId>,
    /// The room's open token, from a full link.
    token: Option<OpenToken>,
    /// Safe to print: never a link's secret.
    shown: String,
}

/// Parse a proxy URL, a redacted link, or a full link.
fn parse_target(target: &str) -> anyhow::Result<Target> {
    if target.starts_with("hya://") || target.starts_with("hya+insecure://") {
        if carries_secret(target) {
            let link = RelayLink::parse(target)
                .map_err(|error| anyhow::anyhow!("invalid relay link: {error}"))?;
            return Ok(Target {
                address: link.address().clone(),
                room: Some(link.room_id().clone()),
                token: Some(link.open_token()),
                shown: link.redacted(),
            });
        }
        let (address, room, _) = RelayLink::parse_public(target)
            .map_err(|error| anyhow::anyhow!("invalid relay link: {error}"))?;
        let shown = format!(
            "{}://{}{}/{room}",
            if address.is_secure() {
                "hya"
            } else {
                "hya+insecure"
            },
            address.authority(),
            address.prefix()
        );
        Ok(Target {
            address,
            room: Some(room),
            token: None,
            shown,
        })
    } else {
        let address = RelayAddress::parse_proxy_url(target)
            .map_err(|error| anyhow::anyhow!("invalid proxy url: {error}"))?;
        Ok(Target {
            address,
            room: None,
            token: None,
            shown: redact_input(target),
        })
    }
}

/// Measure how long an idle stream survives this path (no heartbeats),
/// stepping in 5 s increments up to `cap`. Needs a room with a live host;
/// without one, the measurement reports that the target has none.
async fn measure_idle(
    client: &RelayClient,
    room: Option<&RoomId>,
    token: Option<&OpenToken>,
    cap: Duration,
) -> anyhow::Result<IdleReport> {
    let (Some(room), Some(token)) = (room, token) else {
        anyhow::bail!(
            "--measure-idle opens a stream to the room, which needs the full relay link (its open token); pass it on stdin with `-`"
        );
    };
    let mut transport = client
        .open_with_token(room, token)
        .await
        .map_err(|error| anyhow::anyhow!("cannot open a stream to measure idle time: {error}"))?;
    const STEP: Duration = Duration::from_secs(5);
    let mut elapsed = Duration::ZERO;
    while elapsed < cap {
        tokio::time::sleep(STEP).await;
        elapsed += STEP;
        let probe = Chunk {
            frame: Some(chunk::Frame::Heartbeat(Heartbeat {
                seq: 0,
                pong: false,
            })),
        };
        if transport.send(probe).await.is_err() {
            return Ok(IdleReport {
                cut: true,
                after_secs: elapsed.as_secs(),
            });
        }
        match timeout(Duration::from_secs(3), transport.next()).await {
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(TransportError::Closed))) | Ok(None) | Err(_) => {
                return Ok(IdleReport {
                    cut: true,
                    after_secs: elapsed.as_secs(),
                });
            }
            Ok(Some(Err(_))) => {
                return Ok(IdleReport {
                    cut: true,
                    after_secs: elapsed.as_secs(),
                });
            }
        }
    }
    Ok(IdleReport {
        cut: false,
        after_secs: cap.as_secs(),
    })
}

fn format_report(report: &DoctorReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "target: {}", report.target);
    let _ = writeln!(out, "secure: {}", report.secure);
    write_probe(&mut out, "gRPC", &report.grpc);
    write_probe(&mut out, "WebSocket", &report.ws);
    let _ = writeln!(
        out,
        "path prefix: {}",
        if report.prefix_ok { "ok" } else { "mismatch" }
    );
    match &report.recommended_transport {
        Some(transport) => {
            let _ = writeln!(out, "recommended t={transport}");
        }
        None => {
            let _ = writeln!(out, "recommended: none — no binding works on this path");
        }
    }
    for line in &report.advice {
        let _ = writeln!(out, "advice: {line}");
    }
    if let Some(idle) = &report.idle {
        if idle.cut {
            let _ = writeln!(
                out,
                "idle measurement: the path cut the stream after about {}s without heartbeats",
                idle.after_secs
            );
        } else {
            let _ = writeln!(
                out,
                "idle measurement: survived {}s without a cut (the bound)",
                idle.after_secs
            );
        }
    }
    out
}

fn write_probe(out: &mut String, name: &str, probe: &ProbeReport) {
    use std::fmt::Write as _;
    if probe.ok {
        let _ = writeln!(out, "{name}: ok");
    } else {
        let _ = writeln!(
            out,
            "{name}: failed ({}) {}",
            probe.kind.as_deref().unwrap_or("Unexpected"),
            probe.detail.as_deref().unwrap_or_default()
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Cursor;

    use hya_relay::server::{RelayServer, RelayServerConfig};

    use super::*;

    const ROOM: &str = "eh7ddx5bksrgcytl7bkai36se4";
    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const PSK_B64: &str = "S3CR3TS3CR3TS3CR3TS3CR3TS3CR3TS3CR3TS3CR3TA";

    async fn start_relay() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let config = RelayServerConfig::new("127.0.0.1:0".parse().expect("valid addr"));
        let (addr, serve) = RelayServer::bind(config, std::future::pending())
            .await
            .expect("relay binds");
        (addr, tokio::spawn(serve))
    }

    #[test]
    fn target_resolution_warns_only_for_argv_secret_and_redacts_errors() {
        let redacted = format!("hya+insecure://127.0.0.1:8766/{ROOM}");
        let full = format!("{redacted}#{KEY_B64}.{PSK_B64}");

        let mut stderr = Vec::new();
        assert_eq!(
            resolve_target_from(&redacted, Cursor::new(""), &mut stderr).unwrap(),
            redacted
        );
        assert!(stderr.is_empty());

        assert_eq!(
            resolve_target_from(&full, Cursor::new(""), &mut stderr).unwrap(),
            full
        );
        let warn = String::from_utf8(stderr).unwrap();
        assert!(warn.contains("process listings"), "{warn}");
        assert!(!warn.contains("S3CR3T"), "{warn}");

        let mut stdin_err = Vec::new();
        let from_stdin =
            resolve_target_from("-", Cursor::new(format!("{full}\n")), &mut stdin_err).unwrap();
        assert_eq!(from_stdin, full);
        assert!(stdin_err.is_empty());
        assert!(resolve_target_from("-", Cursor::new("   \n"), &mut Vec::new()).is_err());

        let err = parse_target("https://relay.example.com/x#S3CR3TKEY.S3CR3TPSK")
            .err()
            .expect("malformed proxy url with fragment must fail")
            .to_string();
        assert!(!err.contains("S3CR3T"), "{err}");
    }

    #[tokio::test]
    async fn diagnose_reports_reachable_redacted_full_and_unreachable_relays() {
        let (addr, relay) = start_relay().await;
        let opts = DoctorOptions {
            timeout: Duration::from_secs(3),
            ..DoctorOptions::default()
        };

        let report = diagnose(&format!("http://{addr}"), opts.clone())
            .await
            .unwrap();
        assert!(report.reachable && report.grpc.ok && report.ws.ok && report.prefix_ok);
        assert_eq!(report.recommended_transport.as_deref(), Some("auto"));
        let text = format_report(&report);
        assert!(text.contains(&addr.to_string()) && text.contains("recommended t=auto"));

        let redacted_target = format!("hya+insecure://{addr}/{ROOM}");
        let redacted = diagnose(&redacted_target, opts.clone()).await.unwrap();
        assert_eq!(redacted.target, redacted_target);
        assert!(redacted.reachable);

        let full = diagnose(
            &format!("hya+insecure://{addr}/{ROOM}#{KEY_B64}.{PSK_B64}"),
            opts.clone(),
        )
        .await
        .unwrap();
        let full_text = format_report(&full);
        assert!(full.reachable && !full_text.contains("S3CR3T"));
        assert!(full_text.contains(&redacted_target), "{full_text}");
        relay.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap();
        drop(listener);
        let down = diagnose(&format!("http://{closed}"), opts).await.unwrap();
        assert!(!down.reachable && !down.grpc.ok && !down.ws.ok);
        assert!(down.recommended_transport.is_none());
        assert!(format_report(&down).contains("recommended: none"));
    }

    #[test]
    fn advice_and_report_formatting_cover_failures_and_idle() {
        for (kind, needle) in [
            (ProbeFailureKind::NoHttp2, "t=ws"),
            (ProbeFailureKind::TrailersStripped, "t=ws"),
            (ProbeFailureKind::HopRejected, "t=ws"),
            (ProbeFailureKind::Timeout, "t=ws"),
            (ProbeFailureKind::WrongPath, "--path-prefix"),
            (ProbeFailureKind::Tls, "--relay-ca"),
            (ProbeFailureKind::Connect, "firewall"),
            (ProbeFailureKind::Unexpected, "detail"),
        ] {
            assert!(advice_for(kind).contains(needle), "{kind:?}");
        }
        let report = DoctorReport {
            target: "https://relay.example.com".into(),
            secure: true,
            reachable: true,
            grpc: ProbeReport::failed(&ProbeFailure {
                kind: ProbeFailureKind::NoHttp2,
                detail: "h1 only".into(),
            }),
            ws: ProbeReport::ok(),
            prefix_ok: false,
            recommended_transport: Some("ws".into()),
            advice: vec!["gRPC: use ws".into()],
            idle: Some(IdleReport {
                cut: true,
                after_secs: 25,
            }),
        };
        let rendered = format_report(&report);
        for needle in [
            "gRPC: failed (NoHttp2) h1 only",
            "WebSocket: ok",
            "path prefix: mismatch",
            "recommended t=ws",
            "advice: gRPC: use ws",
            "cut the stream after about 25s",
        ] {
            assert!(rendered.contains(needle), "missing {needle:?}:\n{rendered}");
        }
    }
}
