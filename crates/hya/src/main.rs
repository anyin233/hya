mod args;
mod backend;
mod events;
mod transport;

use std::error::Error;
use std::io::{Error as IoError, ErrorKind, IsTerminal as _};
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
    if let Some(source) = args.import_source.as_deref() {
        return cmd_import(source);
    }
    bootstrap_config_for_frontend(&args)?;
    if !std::io::stdout().is_terminal() {
        println!(
            "hya {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The hya frontend needs a terminal. Try `hya-backend exec \"<prompt>\"`, \
             `hya-backend -p \"<goal>\"`, or `hya-backend --help`."
        );
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

fn cmd_import(source: &str) -> Result<(), Box<dyn Error>> {
    match source.to_ascii_lowercase().as_str() {
        "opencode" => import_opencode_model_config(),
        "codex" | "claude" => Err(invalid_input(format!(
            "hya import from {source} is not supported yet; currently only opencode model import is implemented"
        ))
        .into()),
        _ => Err(invalid_input(format!(
            "unknown import source {source}; currently supported: opencode"
        ))
        .into()),
    }
}

fn import_opencode_model_config() -> Result<(), Box<dyn Error>> {
    let opencode_path = hya_app::config::default_opencode_config_path().ok_or_else(|| {
        invalid_input(
            "no OpenCode config found; set OPENCODE_CONFIG or create ~/.config/opencode/opencode.json",
        )
    })?;
    let config_path = hya_app::config::expected_config_path();
    let summary =
        hya_app::config::import_opencode_models_into_config(&opencode_path, &config_path)?;
    println!(
        "hya: imported {} providers and {} models from OpenCode into {}",
        summary.providers,
        summary.models,
        summary.config_path.display()
    );
    // TODO(import): import OpenCode skills after ownership and merge semantics are defined.
    println!("hya: skills import: TODO");
    // TODO(import): import OpenCode MCP entries after model-only import settles.
    println!("hya: mcp import: TODO");
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> IoError {
    IoError::new(ErrorKind::InvalidInput, message.into())
}

fn bootstrap_config_for_frontend(args: &Args) -> Result<(), Box<dyn Error>> {
    if args.server.is_none() && !args.opencode {
        hya_app::config::first_run_config_bootstrap(true)?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct EnvGuard {
        root: PathBuf,
        home: Option<OsString>,
        xdg_config_home: Option<OsString>,
        opencode_config: Option<OsString>,
    }

    impl EnvGuard {
        fn isolated(prefix: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&root).expect("create temp root");
            let home = root.join("home");
            let xdg = root.join("xdg-config");
            std::fs::create_dir_all(&home).expect("create home");
            std::fs::create_dir_all(&xdg).expect("create xdg config");

            let guard = Self {
                root,
                home: std::env::var_os("HOME"),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                opencode_config: std::env::var_os("OPENCODE_CONFIG"),
            };
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("OPENCODE_CONFIG", xdg.join("missing-opencode.json"));
            guard
        }

        fn config_path(&self) -> PathBuf {
            self.root.join("xdg-config/hya/config.yaml")
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_env("HOME", self.home.take());
            restore_env("XDG_CONFIG_HOME", self.xdg_config_home.take());
            restore_env("OPENCODE_CONFIG", self.opencode_config.take());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn restore_env(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn default_frontend_entry_creates_starter_config_before_tui() {
        let env = EnvGuard::isolated("hya-frontend-first-run-config");
        assert!(!env.config_path().exists(), "test should start clean");

        bootstrap_config_for_frontend(&Args::default()).expect("bootstrap config");

        let config = std::fs::read_to_string(env.config_path()).expect("read created config");
        assert!(
            config.contains("default_model: offline"),
            "created config should contain the offline starter model:\n{config}"
        );
    }
}
