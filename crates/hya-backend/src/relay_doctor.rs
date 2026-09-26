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
    let target = if target == "-" {
        read_stdin_target()?
    } else {
        if carries_secret(&target) {
            eprintln!("warning: {ARGV_WARNING}");
        }
        target
    };
    let report = diagnose(&target, options).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print_report(&report);
    }
    Ok(if report.reachable { 0 } else { 1 })
}

/// Printed when a full link (with its `#` secret) is a command-line argument.
pub(crate) const ARGV_WARNING: &str = "the relay link was given as an argument, so its secret is visible in process listings; `hya relay doctor` only needs the redacted link (drop the `#…` part) or the proxy URL, or pass `-` and write the link to stdin";

/// Whether `target` is a link carrying its secret fragment.
fn carries_secret(target: &str) -> bool {
    target.contains('#')
}

/// The first line of stdin, trimmed.
fn read_stdin_target() -> anyhow::Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| anyhow::anyhow!("reading the target from stdin: {error}"))?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("no relay link or proxy URL on stdin");
    }
    Ok(trimmed.to_owned())
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

fn print_report(report: &DoctorReport) {
    println!("target: {}", report.target);
    println!("secure: {}", report.secure);
    print_probe("gRPC", &report.grpc);
    print_probe("WebSocket", &report.ws);
    println!(
        "path prefix: {}",
        if report.prefix_ok { "ok" } else { "mismatch" }
    );
    match &report.recommended_transport {
        Some(transport) => println!("recommended t={transport}"),
        None => println!("recommended: none — no binding works on this path"),
    }
    for line in &report.advice {
        println!("advice: {line}");
    }
    if let Some(idle) = &report.idle {
        if idle.cut {
            println!(
                "idle measurement: the path cut the stream after about {}s without heartbeats",
                idle.after_secs
            );
        } else {
            println!(
                "idle measurement: survived {}s without a cut (the bound)",
                idle.after_secs
            );
        }
    }
}

fn print_probe(name: &str, probe: &ProbeReport) {
    if probe.ok {
        println!("{name}: ok");
    } else {
        println!(
            "{name}: failed ({}) {}",
            probe.kind.as_deref().unwrap_or("Unexpected"),
            probe.detail.as_deref().unwrap_or_default()
        );
    }
}
