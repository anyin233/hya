mod args;
mod backend;
mod events;
mod transport;

use std::error::Error;
use std::sync::Arc;

use args::{print_usage, Args};
use events::spawn_background_fetches;
use hya_sdk::{PendingClient, PendingSlot};
use hya_tui::app::{run_tui, AppEvent, RunTuiInput};
use hya_tui::state::AppState;
use hya_tui::tui::{install_panic_hook, spawn_input_task, Tui};
use tokio::sync::mpsc;
use transport::Transport;

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
