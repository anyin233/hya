//! Writing commands that go through the server owning their database
//! (docs/cli.md "Database lock and the backend daemon"; [`crate::db_writer`]).
//!
//! When a live server (the backend daemon, or a foreground `hya serve`)
//! holds a file `--db`, `hya exec`/`run` and `hya workflow use|run|state`
//! run over its `/v1` API instead of opening the database a second time. The
//! session is the server's, so every client of that server sees it live.
//! Output and exit statuses follow the direct commands; the differences are
//! documented in docs/cli.md.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use anyhow::Context as _;
use hya_api::v1 as pb;
use hya_core::completion::render_transcript;
use hya_proto::{
    Envelope, Projection, SessionId, WorkflowCommandResult, WorkflowProjection, WorkflowRunResult,
    WorkflowRunStatus,
};
use hya_sdk_v1::{SdkError, V1Sdk};

use crate::StopSignal;
use crate::exec_stream::JsonStreamPrinter;

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

fn client(url: &str) -> anyhow::Result<(V1Sdk, String)> {
    let workdir = std::env::current_dir()
        .context("resolve current directory")?
        .to_string_lossy()
        .into_owned();
    Ok((
        V1Sdk::new(url.trim_end_matches('/'), workdir.clone()),
        workdir,
    ))
}

async fn create_session(
    sdk: &V1Sdk,
    workdir: &str,
    model: Option<String>,
) -> anyhow::Result<String> {
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
        .then(|| JsonStreamPrinter::new(std::io::stdout()));
    let mut since = 0u64;
    let mut asks = Asks::new(id.clone());
    let mut stop: Option<StopSignal> = None;
    let mut idle_rounds = 0u8;
    let info = loop {
        if let Some(printer) = printer.as_mut() {
            since = pump(&sdk, &id, session, since, printer).await?;
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
        pump(&sdk, &id, session, 0, &mut printer).await?;
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
async fn replay(sdk: &V1Sdk, id: &str, since: u64) -> anyhow::Result<(Vec<Envelope>, u64)> {
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
    sdk: &V1Sdk,
    id: &str,
    session: SessionId,
    since: u64,
    printer: &mut JsonStreamPrinter<W>,
) -> anyhow::Result<u64> {
    let (envelopes, next) = replay(sdk, id, since).await?;
    for envelope in &envelopes {
        printer
            .print(envelope, session)
            .context("write json envelope")?;
    }
    printer.flush().context("flush json stream")?;
    Ok(next)
}

/// Rejects the permission asks and questions of one session tree, as the
/// in-process `exec` does for its asks.
struct Asks {
    root: String,
    /// Whether a session belongs to the tree (memoized parent walks).
    in_tree: HashMap<String, bool>,
    answered: HashSet<String>,
}

impl Asks {
    fn new(root: String) -> Self {
        let mut in_tree = HashMap::new();
        in_tree.insert(root.clone(), true);
        Self {
            root,
            in_tree,
            answered: HashSet::new(),
        }
    }

    async fn belongs(&mut self, sdk: &V1Sdk, session: &str) -> bool {
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

    async fn reject_pending(&mut self, sdk: &V1Sdk) {
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

/// A `hya workflow use|run|state` command to run on a server.
pub(crate) enum WorkflowRoute {
    Select {
        name: String,
        expected_revision: Option<String>,
    },
    Run {
        name: Option<String>,
        inputs: BTreeMap<String, String>,
    },
    State,
}

/// Run one workflow command on the server and return the same result the
/// in-process control seam returns. A run without `session` creates a new
/// root session in the current directory; a run is followed until it ends
/// (permission asks and questions of its session tree are rejected).
pub(crate) async fn workflow(
    url: &str,
    session: Option<SessionId>,
    model: Option<String>,
    route: WorkflowRoute,
) -> anyhow::Result<WorkflowCommandResult> {
    let (sdk, workdir) = client(url)?;
    let id = match session {
        Some(session) => session.to_string(),
        None => create_session(&sdk, &workdir, model)
            .await
            .context("create workflow Session")?,
    };
    match route {
        WorkflowRoute::State => {
            let state = sdk.workflow_state(&id).await.map_err(api)?;
            Ok(WorkflowCommandResult::State {
                state: projection(&state)?,
            })
        }
        WorkflowRoute::Select {
            name,
            expected_revision,
        } => {
            let response = submit(
                &sdk,
                serde_json::json!({
                    "session": id,
                    "select": {
                        "name": name,
                        "expectedRevision": expected_revision.unwrap_or_default(),
                    },
                }),
            )
            .await?;
            Ok(WorkflowCommandResult::Selected {
                state: projection(&response)?,
            })
        }
        WorkflowRoute::Run { name, inputs } => {
            let response = submit(
                &sdk,
                serde_json::json!({
                    "session": id,
                    "run": { "name": name.unwrap_or_default(), "inputs": inputs },
                }),
            )
            .await?;
            let started: WorkflowRunResult = serde_json::from_str(&response.raw_json)
                .context("decode the server's Workflow run")?;
            let mut run = started.run;
            // Asks of the run's members are rejected, as in process.
            let mut asks = Asks::new(id.clone());
            while run.status == WorkflowRunStatus::Running {
                asks.reject_pending(&sdk).await;
                tokio::time::sleep(POLL).await;
                let state = sdk.workflow_state(&id).await.map_err(api)?;
                if let Some(latest) = projection(&state)?.run
                    && latest.id == run.id
                {
                    run = latest;
                }
            }
            Ok(WorkflowCommandResult::Run {
                result: WorkflowRunResult {
                    run,
                    replayed: started.replayed,
                },
            })
        }
    }
}

async fn submit(sdk: &V1Sdk, request: serde_json::Value) -> anyhow::Result<pb::WorkflowState> {
    let request: pb::SubmitWorkflowCommandRequest =
        serde_json::from_value(request).context("encode Workflow command")?;
    let response = sdk.submit_workflow_command(request).await.map_err(api)?;
    match response.result {
        Some(
            pb::submit_workflow_command_response::Result::Selected(state)
            | pb::submit_workflow_command_response::Result::Started(state),
        ) => Ok(state),
        _ => anyhow::bail!("the server returned no Workflow state"),
    }
}

fn projection(state: &pb::WorkflowState) -> anyhow::Result<WorkflowProjection> {
    serde_json::from_str(&state.raw_json).context("decode the server's Workflow state")
}
