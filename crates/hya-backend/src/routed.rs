//! Writing commands that go through the server owning their database
//! (docs/cli.md "Database lock and the backend daemon"; [`crate::db_writer`]).
//!
//! When a live server (the backend daemon, or a foreground `hya serve`)
//! holds a file `--db`, `hya exec`/`run` and the `database: true` bundle
//! commands run over its `/v2` API instead of opening the database a second
//! time. The
//! session is the server's, so every client of that server sees it live.
//! Output and exit statuses follow the direct commands; the differences are
//! documented in docs/cli.md.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context as _;
use hya_api::v2 as pb;
use hya_engine::render_transcript;
use hya_proto::{Envelope, Projection, SessionId};
use hya_sdk::{Sdk, SdkError};

use crate::exec_stream::{JsonStreamPrinter, StreamScope};
use crate::stop_signals::StopSignal;

/// How often a routed command polls the server while it waits.
const POLL: Duration = Duration::from_millis(250);

/// A `hya exec`/`run` invocation to run on a server.
pub(crate) struct ExecRequest {
    pub(crate) prompt: String,
    /// `--model`: the new session's model (the server's default otherwise).
    pub(crate) model: Option<String>,
    /// `--yolo`: set the new session's permission mode to `yolo`.
    pub(crate) yolo: bool,
    /// `--json`: print the session's durable envelopes as JSONL.
    pub(crate) json: bool,
}

fn api(error: SdkError) -> anyhow::Error {
    match error {
        SdkError::Api { code, message } => anyhow::anyhow!("{code}: {message}"),
        other => anyhow::anyhow!("{other}"),
    }
}

fn client(url: &str) -> anyhow::Result<(Sdk, String)> {
    let workdir = std::env::current_dir()
        .context("resolve current directory")?
        .to_string_lossy()
        .into_owned();
    Ok((
        Sdk::for_local_daemon(url.trim_end_matches('/'), workdir.clone())?,
        workdir,
    ))
}

async fn create_session(sdk: &Sdk, workdir: &str, model: Option<String>) -> anyhow::Result<String> {
    let session = sdk
        .create_session(pb::CreateSessionRequest {
            model: model.unwrap_or_default(),
            workdir: Some(workdir.to_string()),
            ..Default::default()
        })
        .await
        .map_err(api)
        .context("create session")?;
    anyhow::ensure!(!session.id.is_empty(), "the server created no session");
    Ok(session.id)
}

fn terminal(state: i32) -> bool {
    state == pb::TurnState::Finished as i32
        || state == pb::TurnState::Failed as i32
        || state == pb::TurnState::Cancelled as i32
}

/// Run one prompt turn on the server, like `hya exec` does in process:
/// a new root session in the current directory, the rendered transcript (or
/// the session's durable envelopes with `--json`) on stdout, permission asks
/// and questions of the session tree rejected, SIGINT/SIGTERM cancel the turn
/// and exit 130/143.
pub(crate) async fn exec(url: &str, request: ExecRequest) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (sdk, workdir) = client(url)?;
    let id = create_session(&sdk, &workdir, request.model).await?;
    let session: SessionId = id.parse().context("parse the server's session id")?;
    if request.yolo {
        sdk.update_session(pb::UpdateSessionRequest {
            session: id.clone(),
            permission_mode: Some("yolo".to_string()),
            ..Default::default()
        })
        .await
        .map_err(api)
        .context("set permission mode yolo")?;
    }
    let turn = sdk
        .create_turn(
            &id,
            pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: request.prompt,
                ..Default::default()
            }),
        )
        .await
        .map_err(api)
        .context("admit prompt")?
        .id;
    let mut interrupt = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut printer = request
        .json
        .then(|| JsonStreamPrinter::new(std::io::stdout(), StreamScope::Single(session)));
    let mut since = 0u64;
    let mut asks = Asks::new(id.clone());
    let mut stop: Option<StopSignal> = None;
    let mut idle_rounds = 0u8;
    let info = loop {
        if let Some(printer) = printer.as_mut() {
            since = pump(&sdk, &id, since, printer).await?;
        }
        asks.reject_pending(&sdk).await;
        tokio::select! {
            info = sdk.wait_turn(&id, &turn, POLL) => {
                let info = info.map_err(api).context("wait for the turn")?;
                if terminal(info.state) {
                    break info;
                }
                // The server answers early with a non-terminal state only
                // when the session stayed idle: the turn is over.
                let busy = sdk.get_session(&id).await.map_or(true, |row| row.busy);
                idle_rounds = if busy { 0 } else { idle_rounds.saturating_add(1) };
                if idle_rounds >= 4 {
                    break info;
                }
            }
            _ = interrupt.recv() => {
                if stop.is_some() {
                    eprintln!("hya: interrupted again; exiting (the turn keeps closing on the server)");
                    std::process::exit(StopSignal::Interrupt.exit_code());
                }
                stop = Some(StopSignal::Interrupt);
                eprintln!("hya: stopping — cancelling the turn on the server (Ctrl-C again to exit now)");
                let _ = sdk.cancel_turn(&id, &turn).await;
            }
            _ = terminate.recv() => {
                if stop.is_none() {
                    stop = Some(StopSignal::Terminate);
                    let _ = sdk.cancel_turn(&id, &turn).await;
                }
            }
        }
    };
    let failed = info.state == pb::TurnState::Failed as i32;
    if let Some(mut printer) = printer.take() {
        // Authoritative tail flush from the durable log (deduplicated by seq).
        pump(&sdk, &id, 0, &mut printer).await?;
        printer.flush().context("flush json stream")?;
    } else if !failed {
        let envelopes = replay(&sdk, &id, 0).await?.0;
        print!(
            "{}",
            render_transcript(&Projection::from_events(&envelopes))
        );
    }
    if let Some(stop) = stop {
        crate::exit_if_stopped(Some(stop));
    }
    if failed {
        let reason = if info.error_message.is_empty() {
            info.error_code
        } else {
            info.error_message
        };
        anyhow::bail!("run turn: {reason}");
    }
    if info.state == pb::TurnState::Cancelled as i32 {
        anyhow::bail!("run turn: the turn was cancelled on the server");
    }
    Ok(())
}

/// The session's durable envelopes after `since`, and the next watermark.
async fn replay(sdk: &Sdk, id: &str, since: u64) -> anyhow::Result<(Vec<Envelope>, u64)> {
    let response = sdk
        .list_raw_events(id, since)
        .await
        .map_err(api)
        .context("replay session")?;
    let envelopes = response
        .raw_envelopes
        .iter()
        .map(|line| serde_json::from_str(line).context("decode envelope"))
        .collect::<anyhow::Result<Vec<Envelope>>>()?;
    Ok((envelopes, response.next_seq.max(since)))
}

/// Print the session's new durable envelopes; returns the next watermark.
async fn pump<W: std::io::Write>(
    sdk: &Sdk,
    id: &str,
    since: u64,
    printer: &mut JsonStreamPrinter<W>,
) -> anyhow::Result<u64> {
    let (envelopes, next) = replay(sdk, id, since).await?;
    for envelope in &envelopes {
        printer.print(envelope).context("write json envelope")?;
    }
    printer.flush().context("flush json stream")?;
    Ok(next)
}

/// Rejects the permission asks and questions of one session tree, as the
/// in-process `exec` does for its asks.
pub(crate) struct Asks {
    root: String,
    /// Whether a session belongs to the tree (memoized parent walks).
    in_tree: HashMap<String, bool>,
    answered: HashSet<String>,
}

impl Asks {
    pub(crate) fn new(root: String) -> Self {
        let mut in_tree = HashMap::new();
        in_tree.insert(root.clone(), true);
        Self {
            root,
            in_tree,
            answered: HashSet::new(),
        }
    }

    pub(crate) async fn belongs(&mut self, sdk: &Sdk, session: &str) -> bool {
        let mut chain = Vec::new();
        let mut current = session.to_string();
        let verdict = loop {
            if let Some(known) = self.in_tree.get(&current) {
                break *known;
            }
            if current.is_empty() || chain.len() > 64 {
                break false;
            }
            chain.push(current.clone());
            match sdk.get_session(&current).await {
                Ok(row) if !row.parent.is_empty() => current = row.parent,
                _ => break current == self.root,
            }
        };
        for session in chain {
            self.in_tree.insert(session, verdict);
        }
        verdict
    }

    async fn reject_pending(&mut self, sdk: &Sdk) {
        let Ok(pending) = sdk.list_interactions().await else {
            return;
        };
        for interaction in pending {
            if self.answered.contains(&interaction.id)
                || !self.belongs(sdk, &interaction.session).await
            {
                continue;
            }
            let response = if interaction.r#type == pb::InteractionType::Question as i32 {
                pb::respond_interaction_request::Response::Question(pb::QuestionResponse {
                    answer: String::new(),
                    rejected: true,
                })
            } else {
                pb::respond_interaction_request::Response::Permission(pb::PermissionResponse {
                    allowed: false,
                    persist: false,
                })
            };
            let _ = sdk.respond_interaction(&interaction.id, response).await;
            self.answered.insert(interaction.id);
        }
    }
}

/// A `database: true` bundle CLI call to run on a server.
pub(crate) struct BundleApiRequest<'a> {
    pub(crate) bundle: &'a str,
    pub(crate) api: &'a str,
    /// The `value: session` argument, for a Session-scope endpoint.
    pub(crate) session: Option<SessionId>,
    /// `--model`: the model of a new session.
    pub(crate) model: Option<String>,
    pub(crate) args: serde_json::Value,
}

/// Call a bundle's BundleApi endpoint on the server with `{"args": …,
/// "cwd": …}` and return the reply body. A Session-scope endpoint runs for
/// `session`, else for a new root session in the current directory, and the
/// asks of that session tree are rejected while the call runs; a Global one
/// gets no session. The call has no request timeout.
pub(crate) async fn bundle_api(
    url: &str,
    request: BundleApiRequest<'_>,
) -> anyhow::Result<serde_json::Value> {
    let BundleApiRequest {
        bundle,
        api: id,
        session,
        model,
        args,
    } = request;
    let failed = |error: SdkError| anyhow::anyhow!("{bundle} {id}: {}", api(error));
    let (sdk, workdir) = client(url)?;
    let session = session.map(|session| session.to_string());
    if let Some(session) = &session {
        // A missing session is the caller's error, not the bundle's.
        sdk.get_session(session).await.map_err(api)?;
    }
    let endpoint = sdk
        .list_bundle_apis(session.as_deref())
        .await
        .map_err(failed)?
        .apis
        .into_iter()
        .find(|endpoint| endpoint.bundle == bundle && endpoint.api == id)
        .with_context(|| format!("bundle `{bundle}` serves no API endpoint `{id}`"))?;
    let method = reqwest::Method::from_bytes(endpoint.method.as_bytes())
        .with_context(|| format!("{bundle} {id}: method `{}`", endpoint.method))?;
    let body = serde_json::json!({ "args": args, "cwd": workdir });
    if endpoint.scope != hya_engine::ApiScope::Session.as_str() {
        return sdk
            .invoke_global_bundle_api_unbounded(bundle, method, &endpoint.path, None, Some(&body))
            .await
            .map_err(failed);
    }
    let session = match session {
        Some(session) => session,
        None => create_session(&sdk, &workdir, model).await?,
    };
    let call = sdk.invoke_session_bundle_api_unbounded(
        &session,
        bundle,
        method,
        &endpoint.path,
        None,
        Some(&body),
    );
    let mut asks = Asks::new(session.clone());
    tokio::select! {
        reply = call => reply.map_err(failed),
        never = reject_asks_until_cancelled(&sdk, &mut asks) => match never {},
    }
}

/// Reject the tree's pending asks every [`POLL`]; ends only when dropped.
async fn reject_asks_until_cancelled(sdk: &Sdk, asks: &mut Asks) -> std::convert::Infallible {
    loop {
        asks.reject_pending(sdk).await;
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn daemon_client_bypasses_environment_proxies() -> anyhow::Result<()> {
        // A subprocess isolates proxy variables from the other HTTP tests.
        if std::env::var_os("HYA_TEST_PROXY_CHILD").is_none() {
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command.args([
                "--exact",
                "routed::tests::daemon_client_bypasses_environment_proxies",
                "--nocapture",
            ]);
            command.env("HYA_TEST_PROXY_CHILD", "1");
            for key in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                command.env(key, "http://192.0.2.1:1");
            }
            command.env_remove("NO_PROXY").env_remove("no_proxy");
            let output = command.output()?;
            anyhow::ensure!(
                output.status.success(),
                "proxy child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let app = axum::Router::new()
            .fallback(|| async { axum::Json(serde_json::json!({"id": "local"})) });
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let (sdk, _) = super::client(&url)?;
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(2), sdk.get_session("local")).await;
        server.abort();
        assert_eq!(response??.id, "local");
        Ok(())
    }
}
