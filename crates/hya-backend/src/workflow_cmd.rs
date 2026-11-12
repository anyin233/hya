use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use hya_app::{WorkflowCommand as ControlCommand, WorkflowCommandResult, WorkflowInvocation};
use hya_proto::{
    WorkflowInfo, WorkflowMemberRole, WorkflowModelAssignment, WorkflowModelResolvedCandidate,
    WorkflowProjection, WorkflowRevision, WorkflowRouteFailureClass, WorkflowRunProjection,
    WorkflowRunResult, WorkflowRunStatus, WorkflowStagePlan, WorkflowStageRouteOutcome,
    WorkflowStageStatus, WorkflowSummary,
};
use tokio_util::sync::CancellationToken;

use crate::db_writer::Writer;
use crate::routed::WorkflowRoute;

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

/// Inspect prepared data only: no providers, native tools, MCP or plugin processes.
async fn catalog(db: &str) -> anyhow::Result<hya_app::WorkflowCatalog> {
    use hya_app::{WorkflowCatalog, WorkflowCatalogRoots, bundle_admin};
    use hya_core::{AgentCatalog, CatalogScope, RuntimeRegistry};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    let workdir = std::env::current_dir().context("resolve current directory")?;
    let project = if !db.is_empty() && std::path::Path::new(db).try_exists()? {
        hya_store::SessionStore::connect_read_only(db)
            .await
            .with_context(|| format!("open existing database read-only at {db}"))?
            .resolve_project_by_path(&workdir.to_string_lossy())
            .await?
    } else {
        None
    };
    let scope = project.map_or_else(
        || CatalogScope::Directory(workdir.clone()),
        |project| CatalogScope::Project {
            id: project.id,
            roots: project
                .roots
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect(),
        },
    );
    let registry = bundle_admin::existing_registry().await?;
    let disabled = match &registry {
        Some(registry) => registry.disabled_bundle_ids().await?,
        None => BTreeSet::new(),
    };
    let mut installed = Vec::new();
    if let Some(registry) = registry {
        let snapshot = registry.snapshot().await?;
        for row in snapshot.unreadable {
            eprintln!("warning: installed bundle {}: {}", row.bundle_id, row.error);
        }
        for record in snapshot.bundles {
            installed.push(bundle_admin::decode_installed_catalog(&record)?);
        }
    }
    let roots = match &scope {
        CatalogScope::Project { roots, .. } => roots.as_slice(),
        CatalogScope::Global | CatalogScope::Directory(_) => &[],
    };
    let (bundles, warnings) = hya_app::ProjectScopeRefresh::catalog_for_roots(
        roots,
        &installed,
        &hya_app::first_party_catalogs()?,
        &disabled,
    )?;
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    let agents = AgentCatalog::new(Arc::new(bundles))?;
    let registry = RuntimeRegistry::new(
        hya_tool::ToolRegistry::from_tool_families(&[]),
        Arc::new(agents),
    )
    .with_pure_skills(true);
    let binding = registry.bind_scoped(&scope, &workdir)?;
    WorkflowCatalog::build(WorkflowCatalogRoots::for_scope(&scope, &workdir), &binding)
        .map_err(control_error)
}

/// Run one CLI Workflow command through `WorkflowControl::execute`.
pub(crate) async fn run(
    command: WorkflowCliCommand,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    pure: bool,
) -> anyhow::Result<()> {
    let (command, json_output, is_run, session) = match command {
        WorkflowCliCommand::List { json } => {
            let result = WorkflowCommandResult::List {
                workflows: catalog(db).await?.list(),
            };
            return finish(&result, json, false);
        }
        WorkflowCliCommand::Info { name, json } => {
            let result = WorkflowCommandResult::Info {
                workflow: catalog(db).await?.info(&name).map_err(control_error)?,
            };
            return finish(&result, json, false);
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
    // `list`/`info` only read an existing database. Every other command writes it
    // (a Session, a selection, a run, crash recovery on open), so it holds
    // the database lock, or goes through the server that owns the database.
    let store = match crate::db_writer::claim(db, "workflow").await? {
        Writer::Direct(store) => store,
        Writer::Server(server) => {
            let route = match command {
                ControlCommand::Select {
                    name,
                    expected_revision,
                } => WorkflowRoute::Select {
                    name,
                    expected_revision: expected_revision.map(|revision| revision.to_string()),
                },
                ControlCommand::Run {
                    name,
                    expected_revision,
                    inputs,
                    ..
                } => {
                    let reason = if expected_revision.is_some() {
                        Some("`workflow run --revision` cannot be sent to it")
                    } else if pure {
                        Some("--pure cannot apply to it (it loaded its own context)")
                    } else if yolo {
                        Some("--yolo cannot apply to its Workflow runs")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        crate::db_writer::exit_unroutable("workflow", db, &server, reason);
                    }
                    WorkflowRoute::Run { name, inputs }
                }
                ControlCommand::State => WorkflowRoute::State,
                other => anyhow::bail!("unexpected Workflow command {other:?}"),
            };
            let result =
                crate::routed::workflow(&server.url, session, model_override, route).await?;
            return finish(&result, json_output, is_run);
        }
    };
    let runtime =
        crate::startup::HeadlessSession::start(store, model_override, yolo, pure, session).await?;
    let result = runtime
        .run(async move |runtime| {
            runtime
                .built
                .workflow_control()
                .execute(
                    runtime.session,
                    WorkflowInvocation::default(),
                    command,
                    CancellationToken::new(),
                )
                .await
                .map_err(control_error)
        })
        .await?;
    finish(&result, json_output, is_run)
}

/// Print a Workflow command result; a run that did not complete is an error.
fn finish(result: &WorkflowCommandResult, json_output: bool, is_run: bool) -> anyhow::Result<()> {
    if json_output {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", serde_json::to_string_pretty(result)?)
            .context("write Workflow result")?;
    } else {
        print_text_result(result);
    }
    if is_run && !run_succeeded(result) {
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
        if let Some(assignment) = stage.worker_model.as_ref() {
            print_model_assignment("worker model", assignment);
        }
        if let Some(assignment) = stage.verifier_model.as_ref() {
            print_model_assignment("verifier model", assignment);
        }
    }
}

fn print_model_assignment(label: &str, assignment: &WorkflowModelAssignment) {
    print!(
        "      {label}: {} reasoning={}",
        assignment.id,
        assignment.reasoning.as_deref().unwrap_or("default")
    );
    for candidate in &assignment.fallback {
        print!(
            " -> {} reasoning={}",
            candidate.id,
            candidate.reasoning.as_deref().unwrap_or("default")
        );
    }
    println!();
}

fn print_selected_model(label: &str, selected: &WorkflowModelResolvedCandidate) {
    println!(
        "      selected {label}: #{} {} reasoning={}",
        selected.index, selected.id, selected.reasoning
    );
}

fn print_stage_route_details(plan: &WorkflowStagePlan, outcomes: &[WorkflowStageRouteOutcome]) {
    if let Some(assignment) = plan.worker_model.as_ref() {
        print_model_assignment("worker model", assignment);
    }
    if let Some(selected) = plan.selected_worker_model.as_ref() {
        print_selected_model("worker model", selected);
    }
    if let Some(assignment) = plan.verifier_model.as_ref() {
        print_model_assignment("verifier model", assignment);
    }
    if let Some(selected) = plan.selected_verifier_model.as_ref() {
        print_selected_model("verifier model", selected);
    }
    for outcome in outcomes {
        println!(
            "      route outcome: role={} iteration={} step={} candidate=#{} model={} reasoning={} class={}",
            workflow_member_role_name(outcome.role),
            outcome.iteration,
            outcome.step,
            outcome.candidate_index,
            outcome.model,
            outcome.reasoning,
            route_failure_name(outcome.failure_class)
        );
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
        print_stage_route_details(&stage.plan, &stage.route_outcomes);
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
        print_stage_route_details(&stage.plan, &stage.route_outcomes);
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

fn workflow_member_role_name(role: WorkflowMemberRole) -> &'static str {
    match role {
        WorkflowMemberRole::Worker => "worker",
        WorkflowMemberRole::Verifier => "verifier",
    }
}

fn route_failure_name(class: WorkflowRouteFailureClass) -> &'static str {
    match class {
        WorkflowRouteFailureClass::None => "none",
        WorkflowRouteFailureClass::Transport => "transport",
        WorkflowRouteFailureClass::RateLimited => "rate_limited",
        WorkflowRouteFailureClass::Server => "server",
        WorkflowRouteFailureClass::UnknownModel => "unknown_model",
        WorkflowRouteFailureClass::Auth => "auth",
        WorkflowRouteFailureClass::Incompatible => "incompatible",
        WorkflowRouteFailureClass::Http => "http",
        WorkflowRouteFailureClass::Decode => "decode",
        WorkflowRouteFailureClass::Internal => "internal",
        WorkflowRouteFailureClass::Aborted => "aborted",
        WorkflowRouteFailureClass::Cancelled => "cancelled",
    }
}
