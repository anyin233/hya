use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use hya_app::{
    BuiltSessionEngine, WorkflowCommand as ControlCommand, WorkflowCommandResult, WorkflowControl,
    WorkflowInvocation, agent_with_model, build_session_engine, open_store, resolve_runtime,
};
use hya_core::CreateSession;
use hya_proto::{
    SessionId, WorkflowInfo, WorkflowProjection, WorkflowRevision, WorkflowRunProjection,
    WorkflowRunResult, WorkflowRunStatus, WorkflowStageStatus, WorkflowSummary,
};
use tokio_util::sync::CancellationToken;

/// CLI commands backed by the app-owned Workflow control seam.
#[derive(Subcommand)]
pub(crate) enum WorkflowCliCommand {
    /// List discovered workflows with their declared stages.
    List {
        /// Emit the shared Workflow command result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one workflow's compiled graph and join contract.
    Info {
        /// Workflow `name` as declared in its definition file.
        name: String,
        /// Emit the shared Workflow command result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Persist one selected workflow identity for this Session.
    Use {
        /// Workflow `name` as declared in its definition file.
        name: String,
        /// Optional compiler revision fence (`--expected-revision` is an alias).
        #[arg(long = "revision", alias = "expected-revision")]
        expected_revision: Option<String>,
        /// Existing Session that owns the durable selection.
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Emit the shared Workflow command result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Execute one selected or explicitly named workflow.
    Run {
        /// Workflow `name`, or omit it to use the owning Session's selection.
        name: Option<String>,
        /// Provide one declared input value (`--input key=value`, repeatable).
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
        /// Optional compiler revision fence (`--expected-revision` is an alias).
        #[arg(long = "revision", alias = "expected-revision")]
        expected_revision: Option<String>,
        /// Existing Session that owns the durable run; omitted creates a new Session.
        #[arg(long, value_name = "SESSION_ID")]
        session: Option<String>,
        /// Emit the shared Workflow command result as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the durable selected workflow and latest run state.
    State {
        /// Existing Session whose durable Workflow state is shown.
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Emit the shared Workflow command result as JSON.
        #[arg(long)]
        json: bool,
    },
}

struct WorkflowRuntime {
    built: BuiltSessionEngine,
    control: WorkflowControl,
    session: hya_proto::SessionId,
    responder: tokio::task::JoinHandle<()>,
}

impl WorkflowRuntime {
    async fn start(
        model_override: Option<String>,
        db: &str,
        yolo: bool,
        session: Option<SessionId>,
    ) -> anyhow::Result<Self> {
        let workdir = std::env::current_dir().context("resolve current directory")?;
        crate::first_run_config_bootstrap(false)?;
        let store = open_store(db).await?;
        let runtime = resolve_runtime(model_override).with_yolo(yolo);
        let agent = agent_with_model(&runtime.model, runtime.reasoning);
        let mut built = build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?;
        let engine = built.engine();
        let asks = built
            .take_asks()
            .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
        let _ = built.take_questions();
        let responder = crate::spawn_reject_responder(asks);
        let session = if let Some(session) = session {
            let projection = engine
                .read_projection(session)
                .await
                .context("read workflow Session")?;
            anyhow::ensure!(
                projection.session.id.is_some(),
                "workflow Session `{session}` was not found"
            );
            session
        } else {
            engine
                .create(CreateSession {
                    parent: None,
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                })
                .await
                .context("create workflow Session")?
        };
        let control = built.workflow_control();
        Ok(Self {
            built,
            control,
            session,
            responder,
        })
    }

    async fn execute(
        &self,
        command: ControlCommand,
    ) -> Result<WorkflowCommandResult, hya_app::WorkflowControlError> {
        self.control
            .execute(
                self.session,
                WorkflowInvocation::default(),
                command,
                CancellationToken::new(),
            )
            .await
    }

    async fn shutdown(self) -> anyhow::Result<()> {
        self.responder.abort();
        let mut built = self.built;
        built.shutdown().await.context("shutdown Workflow runtime")
    }
}

/// Run one CLI Workflow command through `WorkflowControl::execute`.
pub(crate) async fn run(
    command: WorkflowCliCommand,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
) -> anyhow::Result<()> {
    let read_only = matches!(
        &command,
        WorkflowCliCommand::List { .. } | WorkflowCliCommand::Info { .. }
    );
    let (command, json_output, is_run, session) = match command {
        WorkflowCliCommand::List { json } => (ControlCommand::List, json, false, None),
        WorkflowCliCommand::Info { name, json } => {
            (ControlCommand::Info { name }, json, false, None)
        }
        WorkflowCliCommand::Use {
            name,
            expected_revision,
            session,
            json,
        } => (
            ControlCommand::Select {
                name,
                expected_revision: parse_revision(expected_revision)?,
            },
            json,
            false,
            Some(session),
        ),
        WorkflowCliCommand::Run {
            name,
            inputs,
            expected_revision,
            session,
            json,
        } => (
            ControlCommand::Run {
                name,
                expected_revision: parse_revision(expected_revision)?,
                inputs: parse_inputs(&inputs)?,
                run: None,
            },
            json,
            true,
            session,
        ),
        WorkflowCliCommand::State { session, json } => {
            (ControlCommand::State, json, false, Some(session))
        }
    };
    anyhow::ensure!(
        !matches!(&command, ControlCommand::Run { name: None, .. }) || session.is_some(),
        "workflow run requires NAME unless --session identifies an owning Session"
    );
    let session = session
        .map(|value| value.parse().context("parse workflow Session id"))
        .transpose()?;
    let runtime = WorkflowRuntime::start(
        model_override,
        if read_only { "" } else { db },
        yolo,
        session,
    )
    .await?;
    let result = runtime.execute(command).await;
    let shutdown = runtime.shutdown().await;
    let result = result.map_err(control_error)?;
    shutdown?;
    if json_output {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", serde_json::to_string_pretty(&result)?)
            .context("write Workflow result")?;
    } else {
        print_text_result(&result);
    }
    if is_run && !run_succeeded(&result) {
        if let WorkflowCommandResult::Run { result } = result {
            anyhow::bail!(
                "workflow `{}` ended {}",
                result.run.workflow.name,
                run_status_name(result.run.status)
            );
        }
        anyhow::bail!("Workflow run returned an unexpected result");
    }
    Ok(())
}

fn control_error(error: hya_app::WorkflowControlError) -> anyhow::Error {
    anyhow::anyhow!("{}: {error}", error.code())
}

fn parse_revision(raw: Option<String>) -> anyhow::Result<Option<WorkflowRevision>> {
    raw.map(|value| {
        value
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid Workflow revision `{value}`: {error}"))
    })
    .transpose()
}

fn parse_inputs(raw: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut inputs = BTreeMap::new();
    for pair in raw {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(anyhow::anyhow!("--input expects KEY=VALUE, got `{pair}`"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(anyhow::anyhow!("--input key must not be empty"));
        }
        if inputs.insert(key.to_string(), value.to_string()).is_some() {
            return Err(anyhow::anyhow!("duplicate --input key `{key}`"));
        }
    }
    Ok(inputs)
}

fn print_text_result(result: &WorkflowCommandResult) {
    match result {
        WorkflowCommandResult::List { workflows } => print_list(workflows),
        WorkflowCommandResult::Info { workflow } => print_info(workflow),
        WorkflowCommandResult::Selected { state } => print_state(state),
        WorkflowCommandResult::State { state } => print_state(state),
        WorkflowCommandResult::Run { result } => print_run(result),
    }
}

fn print_list(workflows: &[WorkflowSummary]) {
    if workflows.is_empty() {
        println!(
            "no workflows found — author Workflow Markdown under .hya/workflows/ (files must end in .hya.md)"
        );
        return;
    }
    for workflow in workflows {
        println!("{}", workflow.name);
        println!("  {}", workflow.path);
        if let Some(error) = &workflow.error {
            println!("  INVALID: {error}");
        } else {
            println!(
                "  {} (Stages: {})",
                workflow.description,
                workflow.stages.join(", ")
            );
        }
    }
}

fn print_info(workflow: &WorkflowInfo) {
    println!("{} — {}", workflow.identity.name, workflow.description);
    println!(
        "failure policy: {} | inputs: {} | revision: {}",
        workflow.on_failure,
        if workflow.inputs.is_empty() {
            "(none)".to_string()
        } else {
            workflow
                .inputs
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        },
        workflow.identity.revision
    );
    for stage in &workflow.stages {
        print!("  {}", stage.id);
        if !stage.predecessors.is_empty() {
            print!(" (after {})", stage.predecessors.join(", "));
        }
        println!(" <- Agent `{}` [{}]", stage.agent, stage.mode);
        if let Some(actor) = &stage.actor {
            println!("      actor: {actor}");
        }
    }
}

fn print_state(state: &WorkflowProjection) {
    match &state.selection {
        Some(selection) => println!(
            "selected workflow: {} (revision {})",
            selection.name, selection.revision
        ),
        None => println!("selected workflow: (none)"),
    }
    if let Some(run) = &state.run {
        print_run_projection(run);
    } else {
        println!("latest run: (none)");
    }
}

fn print_run(result: &WorkflowRunResult) {
    println!(
        "workflow {}: {}{}",
        result.run.workflow.name,
        run_status_name(result.run.status),
        if result.replayed { " (replayed)" } else { "" }
    );
    for stage in &result.run.stages {
        println!(
            "  {} [{}] agent={} members={}",
            stage.plan.id,
            stage_status_name(stage.status),
            stage.plan.agent,
            stage.members.len()
        );
    }
    if let Some(error) = &result.run.error {
        println!("  error: {error}");
    }
}

fn print_run_projection(run: &WorkflowRunProjection) {
    println!(
        "latest run: {} ({})",
        run.workflow.name,
        run_status_name(run.status)
    );
    for stage in &run.stages {
        println!(
            "  {} [{}] agent={} members={}",
            stage.plan.id,
            stage_status_name(stage.status),
            stage.plan.agent,
            stage.members.len()
        );
    }
}

fn run_succeeded(result: &WorkflowCommandResult) -> bool {
    matches!(
        result,
        WorkflowCommandResult::Run {
            result: WorkflowRunResult {
                run: WorkflowRunProjection {
                    status: WorkflowRunStatus::Completed,
                    ..
                },
                ..
            }
        }
    )
}

fn run_status_name(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

fn stage_status_name(status: WorkflowStageStatus) -> &'static str {
    match status {
        WorkflowStageStatus::Pending => "pending",
        WorkflowStageStatus::Running => "running",
        WorkflowStageStatus::Completed => "completed",
        WorkflowStageStatus::Failed => "failed",
        WorkflowStageStatus::Cancelled => "cancelled",
        WorkflowStageStatus::Skipped => "skipped",
    }
}
