use std::error::Error;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use hya_sdk::{
    stream_global_events, ApiClient, Client, GlobalEvent, HttpClient, NativeBridge, PendingClient,
    PendingSlot, ServerHandle,
};
use hya_tui::app::{run_tui, AppEvent, RunTuiInput};
use hya_tui::state::AppState;
use hya_tui::tui::{install_panic_hook, spawn_input_task, Tui};
use hya_yaca::{spawn_event_bridge, YacaNativeTransport};
use tokio::sync::mpsc;
use yaca_app::{RuntimeOptions, YacaRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse()?;
    if args.version {
        println!("hya {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.help {
        print_usage();
        return Ok(());
    }

    install_panic_hook();
    let directory = std::env::current_dir()?.display().to_string();
    let (tx, rx) = mpsc::unbounded_channel();
    let input_task = spawn_input_task(tx.clone());

    // Hand the TUI a not-yet-ready client so it renders and accepts input immediately; the
    // backend (and its possibly-slow MCP servers) connects in the background and is installed
    // into `slot` once ready, at which point queued prompts are released.
    let (client, slot) = PendingClient::create(&directory);
    let connect = spawn_connect(args, directory, tx.clone(), slot);

    let result = run_tui(RunTuiInput {
        tui: Tui::enter()?,
        state: AppState::default(),
        client,
        events: rx,
        tx: tx.clone(),
        input_task: Some(input_task),
        default_agent: None,
        default_model: None,
        agent_names: Vec::new(),
    })
    .await;

    let _ = tx.send(AppEvent::Quit);
    connect.abort();
    if let Ok(Some(transport)) = connect.await {
        transport.shutdown();
    }
    result?;

    Ok(())
}

/// Connect to the backend off the render path: install the real client into `slot`, publish the
/// agent list and MCP status, then signal `BackendReady`. Returns the transport guard so the
/// caller can tear the backend down on exit.
fn spawn_connect(
    args: Args,
    directory: String,
    tx: mpsc::UnboundedSender<AppEvent>,
    slot: Arc<PendingSlot>,
) -> tokio::task::JoinHandle<Option<Transport>> {
    tokio::spawn(async move {
        let profile = std::env::var_os("HYA_PROFILE").is_some();
        let start = std::time::Instant::now();
        let log = |label: &str| {
            if profile {
                eprintln!(
                    "[hya-profile +{:.3}s] {label}",
                    start.elapsed().as_secs_f64()
                );
            }
        };
        log("connect start");
        let _ = tx.send(AppEvent::Toast("Starting backend\u{2026}".to_owned()));
        let (client, transport) = match Transport::connect(&args, &directory, &tx).await {
            Ok(pair) => pair,
            Err(error) => {
                let _ = tx.send(AppEvent::Toast(format!("Backend failed to start: {error}")));
                return None;
            }
        };
        log("backend listening");
        slot.set(Arc::clone(&client));

        let agents = client.agents().await.unwrap_or_default();
        log("agents fetched");
        let agent_list: Vec<(String, Option<(String, String)>)> = agents
            .iter()
            .filter(|agent| !agent.hidden)
            .map(|agent| {
                let model = agent.rest.get("model").and_then(|model| {
                    let provider = model.get("providerID")?.as_str()?;
                    let id = model.get("modelID")?.as_str()?;
                    Some((provider.to_owned(), id.to_owned()))
                });
                (agent.name.clone(), model)
            })
            .collect();
        let default_agent = agent_list.first().map(|(name, _)| name.clone());
        let _ = tx.send(AppEvent::AgentList(agent_list, default_agent));

        if let Ok(status) = client.mcp_status().await {
            let _ = tx.send(AppEvent::McpStatus(status));
        }
        log("mcp status fetched");
        let _ = tx.send(AppEvent::BackendReady);

        spawn_background_fetches(&client, &tx);
        Some(transport)
    })
}

/// Keeps the active backend connection alive for the lifetime of the TUI and tears it down on exit.
enum Transport {
    Yaca {
        runtime: Arc<YacaRuntime>,
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
    /// Connect using the mode implied by `args`: by default run the `yaca` backend IN-PROCESS and
    /// talk to it natively (no TCP, no reqwest). `--http` spawns `yaca serve` and talks HTTP/SSE;
    /// `--server <url>` attaches to an already-running opencode-compatible server; `--opencode`
    /// switches to the opencode backend (native bun bridge, or `opencode serve` over HTTP with
    /// `--http`). Returns the shared client plus the guard that owns the connection.
    async fn connect(
        args: &Args,
        directory: &str,
        tx: &mpsc::UnboundedSender<AppEvent>,
    ) -> Result<(Arc<dyn Client>, Transport), Box<dyn Error + Send + Sync>> {
        if args.server.is_none() && !args.http && !args.opencode {
            let runtime = Arc::new(
                YacaRuntime::start(RuntimeOptions {
                    model: None,
                    db: String::new(),
                    yolo: false,
                    default_agent: None,
                    include_global_agents: true,
                    force_offline: false,
                })
                .await?,
            );
            let transport = YacaNativeTransport::new(runtime.router().clone(), directory);
            let client: Arc<dyn Client> = Arc::new(ApiClient::with_transport(transport));
            let (event_tx, event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
            forward_events(event_rx, tx.clone());
            let bridge =
                spawn_event_bridge(runtime.router().clone(), directory.to_owned(), event_tx);
            return Ok((client, Transport::Yaca { runtime, bridge }));
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

    fn shutdown(self) {
        match self {
            Transport::Yaca { runtime, bridge } => {
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

/// Forward `GlobalEvent`s from the native bridge into the TUI event loop as `AppEvent::Sse`,
/// matching the shape the HTTP SSE path delivers.
fn forward_events(
    mut events: mpsc::UnboundedReceiver<GlobalEvent>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if tx.send(AppEvent::Sse(event)).is_err() {
                break;
            }
        }
    });
}

fn spawn_background_fetches(client: &Arc<dyn Client>, tx: &mpsc::UnboundedSender<AppEvent>) {
    let command_client = Arc::clone(client);
    let command_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(names) = command_client.commands().await {
            let _ = command_tx.send(AppEvent::CommandList(names));
        }
    });

    let model_client = Arc::clone(client);
    let model_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(models) = model_client.models().await {
            let _ = model_tx.send(AppEvent::ModelList(models));
        }
    });

    let mcp_client = Arc::clone(client);
    let mcp_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(status) = mcp_client.mcp_status().await {
                if mcp_tx.send(AppEvent::McpStatus(status)).is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let lsp_client = Arc::clone(client);
    let lsp_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(status) = lsp_client.lsp_status().await {
                if lsp_tx.send(AppEvent::LspStatus(status)).is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let formatter_client = Arc::clone(client);
    let formatter_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(status) = formatter_client.formatter_status().await {
            let _ = formatter_tx.send(AppEvent::FormatterStatus(status));
        }
    });

    let plugin_client = Arc::clone(client);
    let plugin_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(plugins) = plugin_client.plugins().await {
            let _ = plugin_tx.send(AppEvent::PluginList(plugins));
        }
    });
}

fn spawn_sse_task(
    http: reqwest::Client,
    base_url: String,
    directory: String,
    tx: mpsc::UnboundedSender<AppEvent>,
    keep_streaming: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let _ = stream_global_events(&http, &base_url, &directory, |event| {
                let keep_running = keep_streaming.load(Ordering::SeqCst);
                let sent = tx.send(AppEvent::Sse(event)).is_ok();
                keep_running && sent
            })
            .await;
            if !keep_streaming.load(Ordering::SeqCst) {
                break;
            }
            let _ = tx.send(AppEvent::Toast("Reconnecting to server\u{2026}".to_owned()));
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    })
}

/// Locate the vendored backend package that hosts the native bridge script. Overridable with
/// `HYA_BACKEND_DIR`; otherwise resolved relative to this binary's source tree.
fn resolve_backend_dir() -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if let Ok(dir) = std::env::var("HYA_BACKEND_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../opencode-origin/packages/opencode");
    candidate.canonicalize().map_err(|e| {
        format!(
            "cannot locate backend package at {} ({e}); set HYA_BACKEND_DIR",
            candidate.display()
        )
        .into()
    })
}

/// Resolve the `yaca` binary to spawn. Order: `--yaca-bin`, `HYA_YACA_BIN`, the sibling `release`
/// build, a `yaca` on `PATH`, then the sibling `debug` build. Release is preferred over `debug`
/// because the unoptimized debug binary is ~10x larger and far slower to cold-load (the cause of
/// slow backend starts); developers wanting a fresh debug backend can set `HYA_YACA_BIN`.
fn resolve_yaca_bin(args: &Args) -> String {
    if let Some(bin) = &args.yaca_bin {
        return bin.clone();
    }
    if let Ok(bin) = std::env::var("HYA_YACA_BIN") {
        return bin;
    }
    let sibling = |profile: &str| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../../yaca/target/{profile}/yaca"))
            .canonicalize()
            .ok()
            .map(|path| path.display().to_string())
    };
    sibling("release")
        .or_else(yaca_on_path)
        .or_else(|| sibling("debug"))
        .unwrap_or_else(|| "yaca".to_string())
}

/// First `yaca` executable found on `PATH`, if any.
fn yaca_on_path() -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join("yaca");
        candidate.is_file().then(|| candidate.display().to_string())
    })
}

enum ServerMode {
    Spawned(ServerHandle),
    Attached(String),
}

impl ServerMode {
    async fn new(args: &Args, directory: &str) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if let Some(base_url) = args.server.clone() {
            return Ok(Self::Attached(base_url));
        }
        if args.opencode {
            return Ok(Self::Spawned(ServerHandle::spawn(directory).await?));
        }
        let yaca_bin = resolve_yaca_bin(args);
        Ok(Self::Spawned(
            ServerHandle::spawn_yaca(&yaca_bin, directory).await?,
        ))
    }

    fn base_url(&self) -> &str {
        match self {
            Self::Spawned(handle) => handle.base_url(),
            Self::Attached(base_url) => base_url,
        }
    }
}

#[derive(Debug, Default)]
struct Args {
    server: Option<String>,
    http: bool,
    opencode: bool,
    yaca_bin: Option<String>,
    version: bool,
    help: bool,
}

impl Args {
    fn parse() -> Result<Self, std::io::Error> {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => {
                    parsed.server = Some(args.next().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--server requires a URL",
                        )
                    })?);
                }
                "--http" => parsed.http = true,
                "--opencode" => parsed.opencode = true,
                "--yaca-bin" => {
                    parsed.yaca_bin = Some(args.next().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--yaca-bin requires a path",
                        )
                    })?);
                }
                "--version" | "-v" => parsed.version = true,
                "--help" | "-h" => parsed.help = true,
                other => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown argument: {other}"),
                    ));
                }
            }
        }
        Ok(parsed)
    }
}

fn print_usage() {
    println!("usage: hya [OPTIONS]");
    println!(
        "  (default)          run the `yaca` backend in-process and talk to it natively (no HTTP)"
    );
    println!("  --http             spawn `yaca serve` and connect over HTTP/SSE (with --opencode: `opencode serve`)");
    println!(
        "  --server <url>     attach to a running opencode-compatible server (yaca or opencode)"
    );
    println!("  --yaca-bin <path>  yaca binary to spawn for --http (else $HYA_YACA_BIN, sibling build, or PATH)");
    println!("  --opencode         use the opencode backend (native bun bridge) instead of yaca");
    println!("  --version, -v      print version");
    println!("  --help, -h         print this help");
}
