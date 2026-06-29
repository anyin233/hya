use std::error::Error;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use hya_app::{HyaRuntime, RuntimeOptions};
use hya_native::{spawn_event_bridge, HyaNativeTransport};
use hya_sdk::{ApiClient, Client, GlobalEvent, HttpClient, NativeBridge};
use hya_tui::app::AppEvent;
use tokio::sync::mpsc;

use crate::args::Args;
use crate::backend::{resolve_backend_dir, ServerMode};
use crate::events::{forward_events, spawn_sse_task};

/// Keeps the active backend connection alive for the lifetime of the TUI and tears it down on exit.
pub(crate) enum Transport {
    Hya {
        runtime: Arc<HyaRuntime>,
        bridge: tokio::task::JoinHandle<()>,
    },
    Native(NativeBridge),
    Http {
        server: ServerMode,
        sse: tokio::task::JoinHandle<()>,
        keep_streaming: Arc<AtomicBool>,
    },
}

impl Transport {
    /// Connect using the mode implied by `args`: by default run the `hya` backend IN-PROCESS and
    /// talk to it natively (no TCP, no reqwest). `--http` spawns `hya-backend serve` and talks HTTP/SSE;
    /// `--server <url>` attaches to an already-running opencode-compatible server; `--opencode`
    /// switches to the opencode backend (native bun bridge, or `opencode serve` over HTTP with
    /// `--http`). Returns the shared client plus the guard that owns the connection.
    pub(crate) async fn connect(
        args: &Args,
        directory: &str,
        tx: &mpsc::UnboundedSender<AppEvent>,
    ) -> Result<(Arc<dyn Client>, Transport), Box<dyn Error + Send + Sync>> {
        if args.server.is_none() && !args.http && !args.opencode {
            let runtime = Arc::new(
                HyaRuntime::start(RuntimeOptions {
                    model: None,
                    db: String::new(),
                    yolo: false,
                    default_agent: None,
                    include_global_agents: true,
                    force_offline: false,
                })
                .await?,
            );
            let transport = HyaNativeTransport::new(runtime.router().clone(), directory);
            let client: Arc<dyn Client> = Arc::new(ApiClient::with_transport(transport));
            let (event_tx, event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
            forward_events(event_rx, tx.clone());
            let bridge =
                spawn_event_bridge(runtime.router().clone(), directory.to_owned(), event_tx);
            return Ok((client, Transport::Hya { runtime, bridge }));
        }

        if args.opencode && !args.http && args.server.is_none() {
            let (event_tx, event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
            let bridge = NativeBridge::spawn(&resolve_backend_dir()?, event_tx).await?;
            let client: Arc<dyn Client> = Arc::new(bridge.client(directory));
            forward_events(event_rx, tx.clone());
            return Ok((client, Transport::Native(bridge)));
        }

        let http = reqwest::Client::new();
        let server = ServerMode::new(args, directory).await?;
        let client: Arc<dyn Client> = Arc::new(HttpClient::with_http(
            http.clone(),
            server.base_url(),
            directory,
        ));
        let keep_streaming = Arc::new(AtomicBool::new(true));
        let sse = spawn_sse_task(
            http,
            server.base_url().to_owned(),
            directory.to_owned(),
            tx.clone(),
            Arc::clone(&keep_streaming),
        );
        Ok((
            client,
            Transport::Http {
                server,
                sse,
                keep_streaming,
            },
        ))
    }

    pub(crate) fn shutdown(self) {
        match self {
            Transport::Hya { runtime, bridge } => {
                bridge.abort();
                drop(runtime);
            }
            Transport::Native(bridge) => drop(bridge),
            Transport::Http {
                server,
                sse,
                keep_streaming,
            } => {
                keep_streaming.store(false, Ordering::SeqCst);
                sse.abort();
                drop(server);
            }
        }
    }
}
