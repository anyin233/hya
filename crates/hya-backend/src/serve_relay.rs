//! The relay side of `hya serve` (ADR-0025; docs/relay.md "Hosting a
//! backend on a relay"): turning `--relay …` flags into connector settings,
//! recording them in the discovery file so `hya serve restart` rejoins, and
//! the `hya serve relay connect|disconnect|status|link|rotate` client of the
//! running backend's loopback-only `RelayControl` rpcs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use hya_server::relay_host::{RelayHostConfig, RelaySettings, parse_proxy_url};
use serde_json::{Value, json};

use crate::cli_args::{RelayFlags, ServeRelayAction};
use crate::db_lock::{self, DiscoveredRelay};

/// Exit status of `hya serve relay …` when no server runs or an rpc fails.
pub(crate) const EXIT_RELAY_FAILED: i32 = 1;

/// The marker in front of the relay link `hya serve --relay` prints once on
/// stderr (and `hya serve start|restart --relay`).
pub(crate) const LINK_MARKER: &str = "hya relay link:";

/// `path` made absolute against the current directory.
fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .context("read the current directory")?
        .join(path))
}

/// The same flags with an absolute `--relay-ca` (a daemon runs in `$HOME`).
pub(crate) fn absolutize(mut flags: RelayFlags) -> anyhow::Result<RelayFlags> {
    if let Some(ca) = &flags.relay_ca {
        flags.relay_ca = Some(absolute(ca)?);
    }
    Ok(flags)
}

/// Connector settings of `--relay …`; `None` without `--relay`. Validates the
/// URL and the CA file before anything starts.
pub(crate) fn settings(flags: &RelayFlags) -> anyhow::Result<Option<RelaySettings>> {
    let Some(url) = &flags.relay else {
        return Ok(None);
    };
    parse_proxy_url(url).map_err(|error| anyhow::anyhow!("--relay: {error}"))?;
    let extra_ca = match &flags.relay_ca {
        Some(ca) => {
            let ca = absolute(ca)?;
            anyhow::ensure!(
                ca.is_file(),
                "--relay-ca: {} is not a readable file",
                ca.display()
            );
            Some(ca)
        }
        None => None,
    };
    let transport = flags
        .relay_transport
        .as_deref()
        .unwrap_or("auto")
        .parse()
        .map_err(|_| anyhow::anyhow!("--relay-transport must be auto, grpc, or ws"))?;
    Ok(Some(RelaySettings {
        proxy_url: url.trim().to_owned(),
        transport,
        extra_ca,
        ephemeral: flags.relay_ephemeral,
    }))
}

/// The connector's host configuration for `db` (identity file next to the
/// database; ephemeral for an in-memory one).
pub(crate) fn host_config(db: &str, flags: &RelayFlags) -> RelayHostConfig {
    let mut config = RelayHostConfig {
        identity_path: db_lock::paths(db).map(|paths| paths.relay_identity),
        ..RelayHostConfig::default()
    };
    if let Some(seconds) = flags.relay_heartbeat {
        config.heartbeat = hya_relay::client::HeartbeatConfig::every(Duration::from_secs(seconds));
    }
    config
}

/// What the discovery file records for `settings`.
pub(crate) fn discovered(settings: &RelaySettings, heartbeat: Option<u64>) -> DiscoveredRelay {
    DiscoveredRelay {
        proxy_url: settings.proxy_url.clone(),
        transport: settings.transport.as_str().to_owned(),
        ca: settings.extra_ca.clone(),
        ephemeral: settings.ephemeral,
        heartbeat_secs: heartbeat,
    }
}

/// The flags that rejoin a recorded relay.
pub(crate) fn flags_of(relay: &DiscoveredRelay) -> RelayFlags {
    RelayFlags {
        relay: Some(relay.proxy_url.clone()),
        relay_transport: Some(relay.transport.clone()).filter(|transport| transport != "auto"),
        relay_ca: relay.ca.clone(),
        relay_ephemeral: relay.ephemeral,
        relay_heartbeat: relay.heartbeat_secs,
        relay_quiet_link: false,
    }
}

/// The relay flags of the daemon `hya serve restart` starts: the ones given
/// to `restart`, else the relay the old backend was joined to.
pub(crate) fn restart_flags(explicit: RelayFlags, old: Option<&DiscoveredRelay>) -> RelayFlags {
    if explicit.relay.is_some() {
        return explicit;
    }
    match old {
        Some(old) => {
            let mut flags = flags_of(old);
            if explicit.relay_heartbeat.is_some() {
                flags.relay_heartbeat = explicit.relay_heartbeat;
            }
            flags
        }
        None => explicit,
    }
}

/// Print the link once, clearly marked, on stderr: it is a secret.
pub(crate) fn print_link(link: &str) {
    eprintln!("{LINK_MARKER} {link}");
    eprintln!(
        "hya: the relay link is a secret: anyone holding it controls this backend (rotate it with `hya serve relay rotate`)"
    );
}

/// The running server of `db`, or exit 1 with the `status` message.
async fn running_url(db: &str) -> String {
    match crate::daemon::running(db).await {
        Some(found) => found.url,
        None => {
            eprintln!("no hya server is running on {db}");
            std::process::exit(EXIT_RELAY_FAILED);
        }
    }
}

/// Call one `RelayControl` rpc on the server at `url`.
pub(crate) async fn call(
    url: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build the HTTP client")?;
    let mut request = client.request(method, format!("{url}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("call {url}{path}"))?;
    let status = response.status();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = value["error"]["message"]
            .as_str()
            .map_or_else(|| format!("HTTP {status}"), str::to_owned);
        anyhow::bail!("{message}");
    }
    Ok(value)
}

/// `RELAY_STATE_CONNECTED` → `connected`.
fn state_word(status: &Value) -> String {
    status["state"]
        .as_str()
        .and_then(|state| state.strip_prefix("RELAY_STATE_"))
        .unwrap_or("DISCONNECTED")
        .to_ascii_lowercase()
}

/// The human `hya serve relay status` lines.
pub(crate) fn status_lines(status: &Value) -> Vec<String> {
    let state = state_word(status);
    let text = |key: &str| status[key].as_str().filter(|value| !value.is_empty());
    let mut lines = vec![format!("relay {state}")];
    if let Some(proxy) = text("proxy") {
        lines.push(format!("  proxy      {proxy}"));
    }
    if let Some(room) = text("roomId") {
        lines.push(format!("  room       {room}"));
    }
    if let Some(link) = text("redactedLink") {
        lines.push(format!(
            "  link       {link}#… (secret part hidden: `hya serve relay link`)"
        ));
    }
    if let Some(transport) = text("transport") {
        let binding = match (text("binding"), text("bindingReason")) {
            (Some(binding), Some(reason)) => format!(", using {binding} ({reason})"),
            (Some(binding), None) => format!(", using {binding}"),
            _ => String::new(),
        };
        lines.push(format!("  transport  {transport}{binding}"));
    }
    if let Some(since) = text("connectedSince") {
        lines.push(format!("  since      {since}"));
    }
    if state != "disconnected" {
        lines.push(format!(
            "  streams    {}",
            status["activeStreams"].as_u64().unwrap_or(0)
        ));
        let identity = if status["ephemeral"].as_bool().unwrap_or(false) {
            "ephemeral (the link dies with this connection)"
        } else {
            "persistent (<db>.relay-identity.json)"
        };
        lines.push(format!("  identity   {identity}"));
    }
    if let Some(error) = text("lastError") {
        lines.push(format!("  error      {error}"));
    }
    lines
}

/// `hya serve relay …` against the running backend of `db`.
pub(crate) async fn run(action: ServeRelayAction, db: &str) -> anyhow::Result<()> {
    let url = running_url(db).await;
    let outcome = match action {
        ServeRelayAction::Connect {
            proxy_url,
            transport,
            relay_ca,
            ephemeral,
            json,
        } => {
            let ca = relay_ca.as_deref().map(absolute).transpose()?;
            let body = json!({
                "proxyUrl": proxy_url,
                "transport": transport,
                "extraCaPath": ca.map(|ca| ca.to_string_lossy().into_owned()).unwrap_or_default(),
                "ephemeral": ephemeral,
            });
            call(&url, reqwest::Method::POST, "/v1/relay/connect", Some(body))
                .await
                .map(|value| {
                    if json {
                        println!("{value}");
                    } else {
                        for line in status_lines(&value["status"]) {
                            println!("{line}");
                        }
                        println!("{LINK_MARKER} {}", value["link"].as_str().unwrap_or(""));
                        eprintln!(
                            "hya: the relay link is a secret: anyone holding it controls this backend"
                        );
                    }
                })
        }
        ServeRelayAction::Disconnect { json } => call(
            &url,
            reqwest::Method::POST,
            "/v1/relay/disconnect",
            Some(json!({})),
        )
        .await
        .map(|value| {
            if json {
                println!("{value}");
            } else {
                println!("relay {}", state_word(&value));
            }
        }),
        ServeRelayAction::Status { json } => {
            call(&url, reqwest::Method::GET, "/v1/relay/status", None)
                .await
                .map(|value| {
                    if json {
                        println!("{value}");
                    } else {
                        for line in status_lines(&value) {
                            println!("{line}");
                        }
                    }
                })
        }
        ServeRelayAction::Link => call(&url, reqwest::Method::GET, "/v1/relay/link", None)
            .await
            .map(|value| println!("{}", value["link"].as_str().unwrap_or(""))),
        ServeRelayAction::Rotate { json } => call(
            &url,
            reqwest::Method::POST,
            "/v1/relay/rotate",
            Some(json!({})),
        )
        .await
        .map(|value| {
            if json {
                println!("{value}");
            } else {
                match value["link"].as_str().filter(|link| !link.is_empty()) {
                    Some(link) => {
                        println!("{LINK_MARKER} {link}");
                        eprintln!(
                            "hya: rotated the relay key: every earlier link is revoked and open relay connections were closed"
                        );
                    }
                    None => println!(
                        "rotated the relay key (not connected to a relay: `hya serve relay connect <url>` prints the new link)"
                    ),
                }
            }
        }),
    };
    if let Err(error) = outcome {
        eprintln!("hya serve relay: {error:#}");
        std::process::exit(EXIT_RELAY_FAILED);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn settings_validate_the_url_and_default_to_auto() {
        assert!(settings(&RelayFlags::default()).unwrap().is_none());
        let flags = RelayFlags {
            relay: Some("https://relay.example.com/hya".into()),
            ..RelayFlags::default()
        };
        let parsed = settings(&flags).unwrap().unwrap();
        assert_eq!(parsed.proxy_url, "https://relay.example.com/hya");
        assert_eq!(parsed.transport.as_str(), "auto");
        assert!(!parsed.ephemeral);
        let bad = RelayFlags {
            relay: Some("ftp://relay".into()),
            ..RelayFlags::default()
        };
        assert!(settings(&bad).unwrap_err().to_string().contains("--relay"));
        let missing_ca = RelayFlags {
            relay: Some("https://relay.example.com".into()),
            relay_ca: Some("/nonexistent/ca.pem".into()),
            ..RelayFlags::default()
        };
        assert!(settings(&missing_ca).is_err());
    }

    #[test]
    fn restart_rejoins_the_recorded_relay_unless_told_otherwise() {
        let old = DiscoveredRelay {
            proxy_url: "https://relay.example.com".into(),
            transport: "ws".into(),
            ca: Some("/etc/ca.pem".into()),
            ephemeral: false,
            heartbeat_secs: Some(20),
        };
        let flags = restart_flags(RelayFlags::default(), Some(&old));
        assert_eq!(flags.relay.as_deref(), Some("https://relay.example.com"));
        assert_eq!(flags.relay_transport.as_deref(), Some("ws"));
        assert_eq!(flags.relay_ca.as_deref(), Some(Path::new("/etc/ca.pem")));
        assert_eq!(flags.relay_heartbeat, Some(20));
        let explicit = RelayFlags {
            relay: Some("http://100.64.0.7:8766".into()),
            ..RelayFlags::default()
        };
        assert_eq!(restart_flags(explicit.clone(), Some(&old)), explicit);
        assert_eq!(
            restart_flags(RelayFlags::default(), None),
            RelayFlags::default()
        );
        // Recording the settings of those flags gives the same record back.
        let parsed = settings(&RelayFlags {
            relay_ca: None,
            ..flags
        })
        .unwrap()
        .unwrap();
        let recorded = discovered(&parsed, Some(20));
        assert_eq!(recorded.proxy_url, old.proxy_url);
        assert_eq!(recorded.transport, "ws");
        assert_eq!(recorded.heartbeat_secs, Some(20));
    }

    #[test]
    fn status_lines_never_show_a_secret_and_name_the_state() {
        let status = json!({
            "state": "RELAY_STATE_CONNECTED",
            "proxy": "https://relay.example.com",
            "roomId": "eh7ddx5bksrgcytl7bkai36se4",
            "redactedLink": "hya://relay.example.com/eh7ddx5bksrgcytl7bkai36se4",
            "transport": "auto",
            "binding": "grpc",
            "bindingReason": "gRPC works on this path",
            "activeStreams": 2,
        });
        let lines = status_lines(&status).join("\n");
        assert!(lines.starts_with("relay connected"), "{lines}");
        assert!(lines.contains("using grpc"), "{lines}");
        assert!(lines.contains("streams    2"), "{lines}");
        assert!(lines.contains("persistent"), "{lines}");
        assert_eq!(status_lines(&json!({})), ["relay disconnected"]);
    }
}
