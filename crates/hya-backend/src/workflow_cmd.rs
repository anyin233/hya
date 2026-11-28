use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use hya_app::{WorkflowCommand as ControlCommand, WorkflowCommandResult, WorkflowInvocation};
use hya_proto::{
    WorkflowInfo, WorkflowModelAssignment, WorkflowModelResolvedCandidate, WorkflowRevision,
    WorkflowSummary,
};
use hya_tool::{
    WorkflowMemberRole, WorkflowProjection, WorkflowRouteFailureClass, WorkflowRunProjection,
    WorkflowRunResult, WorkflowRunStatus, WorkflowStagePlan, WorkflowStageRouteOutcome,
    WorkflowStageStatus,
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

async fn catalog(db: &str) -> anyhow::Result<hya_app::WorkflowCatalog> {
    let workdir = std::env::current_dir().context("resolve current directory")?;
    hya_app::WorkflowCatalog::inspect_read_only(db, &workdir, |warning| {
        eprintln!("warning: {warning}");
    })
    .await
}

/// Run one CLI Workflow command through `WorkflowControl::execute`.
pub(crate) async fn run(
    command: WorkflowCliCommand,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    bare: bool,
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
    let claimed = if is_run && session.is_none() {
        crate::db_writer::claim_or_memory(db, "workflow").await?
    } else {
        crate::db_writer::claim(db, "workflow").await?
    };
    let store = match claimed {
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
                    } else if bare {
                        Some("--bare cannot apply to it (it loaded its own bundle profile)")
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
    let exit_store = store.clone();
    let mut signals = crate::stop_signals::StopSignals::install()?;
    let mut runtime = crate::stop_signals::start_headless_with_signals(
        &mut signals,
        store,
        model_override,
        yolo,
        bare,
        pure,
        session,
    )
    .await?;
    let cancel = CancellationToken::new();
    let (result, stop) = crate::stop_signals::run_until_stopped(
        &runtime.built,
        &mut signals,
        &cancel,
        runtime.built.workflow_control().execute(
            runtime.session,
            WorkflowInvocation::default(),
            command,
            cancel.clone(),
        ),
    )
    .await;
    let shutdown_res = signals
        .finish_with_store_report(&exit_store, stop.is_some(), runtime.shutdown())
        .await;
    shutdown_res?;
    crate::stop_signals::exit_if_stopped(stop);
    let result = result.map_err(control_error)?;
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
    print!("{}", format_text_result(result));
}

fn format_text_result(result: &WorkflowCommandResult) -> String {
    let mut out = String::new();
    match result {
        WorkflowCommandResult::List { workflows } => write_list(&mut out, workflows),
        WorkflowCommandResult::Info { workflow } => write_info(&mut out, workflow),
        WorkflowCommandResult::Selected { state } => write_state(&mut out, state),
        WorkflowCommandResult::State { state } => write_state(&mut out, state),
        WorkflowCommandResult::Run { result } => write_run(&mut out, result),
    }
    out
}

fn write_list(out: &mut String, workflows: &[WorkflowSummary]) {
    use std::fmt::Write as _;
    if workflows.is_empty() {
        let _ = writeln!(
            out,
            "no workflows found — author Workflow Markdown under .hya/workflows/ (files must end in .hya.md)"
        );
        return;
    }
    for workflow in workflows {
        let _ = writeln!(out, "{}", workflow.name);
        let _ = writeln!(out, "  {}", workflow.path);
        if let Some(error) = &workflow.error {
            let _ = writeln!(out, "  INVALID: {error}");
        } else {
            let _ = writeln!(
                out,
                "  {} (Stages: {})",
                workflow.description,
                workflow.stages.join(", ")
            );
        }
    }
}

fn write_info(out: &mut String, workflow: &WorkflowInfo) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "{} — {}", workflow.identity.name, workflow.description);
    let _ = writeln!(
        out,
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
        let _ = write!(out, "  {}", stage.id);
        if !stage.predecessors.is_empty() {
            let _ = write!(out, " (after {})", stage.predecessors.join(", "));
        }
        let _ = writeln!(out, " <- Agent `{}` [{}]", stage.agent, stage.mode);
        if let Some(actor) = &stage.actor {
            let _ = writeln!(out, "      actor: {actor}");
        }
        if let Some(assignment) = stage.worker_model.as_ref() {
            write_model_assignment(out, "worker model", assignment);
        }
        if let Some(assignment) = stage.verifier_model.as_ref() {
            write_model_assignment(out, "verifier model", assignment);
        }
    }
}

fn write_model_assignment(out: &mut String, label: &str, assignment: &WorkflowModelAssignment) {
    use std::fmt::Write as _;
    let _ = write!(
        out,
        "      {label}: {} reasoning={}",
        assignment.id,
        assignment.reasoning.as_deref().unwrap_or("default")
    );
    for candidate in &assignment.fallback {
        let _ = write!(
            out,
            " -> {} reasoning={}",
            candidate.id,
            candidate.reasoning.as_deref().unwrap_or("default")
        );
    }
    let _ = writeln!(out);
}

fn write_selected_model(out: &mut String, label: &str, selected: &WorkflowModelResolvedCandidate) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "      selected {label}: #{} {} reasoning={}",
        selected.index, selected.id, selected.reasoning
    );
}

fn write_stage_route_details(
    out: &mut String,
    plan: &WorkflowStagePlan,
    outcomes: &[WorkflowStageRouteOutcome],
) {
    use std::fmt::Write as _;
    if let Some(assignment) = plan.worker_model.as_ref() {
        write_model_assignment(out, "worker model", assignment);
    }
    if let Some(selected) = plan.selected_worker_model.as_ref() {
        write_selected_model(out, "worker model", selected);
    }
    if let Some(assignment) = plan.verifier_model.as_ref() {
        write_model_assignment(out, "verifier model", assignment);
    }
    if let Some(selected) = plan.selected_verifier_model.as_ref() {
        write_selected_model(out, "verifier model", selected);
    }
    for outcome in outcomes {
        let _ = writeln!(
            out,
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

fn write_state(out: &mut String, state: &WorkflowProjection) {
    use std::fmt::Write as _;
    match &state.selection {
        Some(selection) => {
            let _ = writeln!(
                out,
                "selected workflow: {} (revision {})",
                selection.name, selection.revision
            );
        }
        None => {
            let _ = writeln!(out, "selected workflow: (none)");
        }
    }
    if let Some(run) = &state.run {
        write_run_projection(out, run);
    } else {
        let _ = writeln!(out, "latest run: (none)");
    }
}

fn write_run(out: &mut String, result: &WorkflowRunResult) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "workflow {}: {}{}",
        result.run.workflow.name,
        run_status_name(result.run.status),
        if result.replayed { " (replayed)" } else { "" }
    );
    for stage in &result.run.stages {
        let _ = writeln!(
            out,
            "  {} [{}] agent={} members={}",
            stage.plan.id,
            stage_status_name(stage.status),
            stage.plan.agent,
            stage.members.len()
        );
        write_stage_route_details(out, &stage.plan, &stage.route_outcomes);
    }
    if let Some(error) = &result.run.error {
        let _ = writeln!(out, "  error: {error}");
    }
}

fn write_run_projection(out: &mut String, run: &WorkflowRunProjection) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "latest run: {} ({})",
        run.workflow.name,
        run_status_name(run.status)
    );
    for stage in &run.stages {
        let _ = writeln!(
            out,
            "  {} [{}] agent={} members={}",
            stage.plan.id,
            stage_status_name(stage.status),
            stage.plan.agent,
            stage.members.len()
        );
        write_stage_route_details(out, &stage.plan, &stage.route_outcomes);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use hya_proto::{
        AgentName, MemberId, ModelRef, OwnerRunId, SessionId, WorkflowIdentity,
        WorkflowModelCandidate, WorkflowRunId, WorkflowSourceId, WorkflowStageInfo,
    };
    use hya_tool::WorkflowStageProjection;

    use super::*;

    const ECHO_WF: &str = "---\nkind: Workflow\nname: echo-input\ndescription: Echo.\ninputs:\n  v: val\nnodes:\n  capture:\n    agent: hya-task\n    directive: captured={{input.v}}\n---\nflowchart TD\n  capture\n";

    #[test]
    fn parses_inputs_and_revisions_and_rejects_invalid_forms() {
        let parsed = parse_inputs(&["v=a=b=c&d=e".into(), " x = kept ".into()]).unwrap();
        assert_eq!(parsed.get("v").map(String::as_str), Some("a=b=c&d=e"));
        assert_eq!(parsed.get("x").map(String::as_str), Some(" kept "));
        for (raw, needle) in [
            (vec!["missing_eq".into()], "KEY=VALUE"),
            (vec!["  =val".into()], "must not be empty"),
            (
                vec!["v=1".into(), "v=2".into()],
                "duplicate --input key `v`",
            ),
        ] {
            assert!(parse_inputs(&raw).unwrap_err().to_string().contains(needle));
        }
        assert!(parse_revision(None).unwrap().is_none());
        let hex = "a".repeat(64);
        assert_eq!(
            parse_revision(Some(hex.clone()))
                .unwrap()
                .unwrap()
                .to_string(),
            hex
        );
        assert!(parse_revision(Some("not-hex".into())).is_err());
    }

    #[tokio::test]
    async fn run_without_name_or_session_fails_before_touching_database() {
        let dir = std::env::temp_dir().join(format!("hya-wf-nameless-{}", SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let err = run(
            WorkflowCliCommand::Run {
                name: None,
                inputs: vec!["v=1".into()],
                expected_revision: None,
                session: None,
                json: false,
            },
            None,
            db.to_str().unwrap(),
            false,
            false,
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("requires NAME unless --session") && !db.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_list_info_run_and_state_render_expected_lines() {
        assert!(
            format_text_result(&WorkflowCommandResult::List { workflows: vec![] })
                .contains("no workflows found")
        );
        let list = format_text_result(&WorkflowCommandResult::List {
            workflows: vec![
                WorkflowSummary {
                    name: "ok".into(),
                    source: Some(WorkflowSourceId::new("project:ok")),
                    path: ".hya/workflows/ok.hya.md".into(),
                    description: "Works".into(),
                    revision: Some("a".repeat(64).parse().unwrap()),
                    stages: vec!["a".into(), "b".into()],
                    error: None,
                },
                WorkflowSummary {
                    name: "bad".into(),
                    source: None,
                    path: ".hya/workflows/bad.hya.md".into(),
                    description: String::new(),
                    revision: None,
                    stages: vec![],
                    error: Some("parse failed".into()),
                },
            ],
        });
        assert!(list.contains("Works (Stages: a, b)") && list.contains("INVALID: parse failed"));

        let identity = WorkflowIdentity {
            name: "routed".into(),
            source: WorkflowSourceId::new("project:routed"),
            revision: "b".repeat(64).parse().unwrap(),
        };
        let assignment = WorkflowModelAssignment {
            id: "hya/offline".into(),
            reasoning: None,
            fallback: vec![WorkflowModelCandidate {
                id: "openai/gpt".into(),
                reasoning: Some("high".into()),
            }],
        };
        let info = format_text_result(&WorkflowCommandResult::Info {
            workflow: WorkflowInfo {
                identity: identity.clone(),
                path: ".hya/workflows/routed.hya.md".into(),
                description: "Routed".into(),
                on_failure: "halt".into(),
                inputs: BTreeMap::from([("topic".into(), "desc".into())]),
                stages: vec![WorkflowStageInfo {
                    id: "execute".into(),
                    title: None,
                    agent: "hya-task".into(),
                    level: 0,
                    predecessors: vec!["plan".into()],
                    actor: Some("lead".into()),
                    mode: "once".into(),
                    worker_model: Some(assignment.clone()),
                    verifier_model: None,
                }],
            },
        });
        for needle in [
            "routed — Routed",
            "inputs: topic",
            "execute (after plan) <- Agent `hya-task` [once]",
            "actor: lead",
            "worker model: hya/offline reasoning=default -> openai/gpt reasoning=high",
        ] {
            assert!(info.contains(needle), "missing {needle:?}:\n{info}");
        }

        let run_id = WorkflowRunId::new();
        let run_res = WorkflowCommandResult::Run {
            result: WorkflowRunResult {
                replayed: true,
                run: WorkflowRunProjection {
                    id: run_id,
                    workflow: identity,
                    request_hash: "hash".into(),
                    owner: OwnerRunId::new(),
                    status: WorkflowRunStatus::Completed,
                    stages: vec![WorkflowStageProjection {
                        plan: WorkflowStagePlan {
                            id: "execute".into(),
                            title: None,
                            agent: AgentName::new("hya-task"),
                            mode: "once".into(),
                            level: 0,
                            worker_model: Some(assignment),
                            selected_worker_model: Some(WorkflowModelResolvedCandidate {
                                index: 0,
                                id: "hya/offline".into(),
                                reasoning: "none".into(),
                            }),
                            verifier_model: None,
                            selected_verifier_model: None,
                        },
                        status: WorkflowStageStatus::Completed,
                        members: vec![],
                        route_outcomes: vec![WorkflowStageRouteOutcome {
                            session: SessionId::new(),
                            run: run_id,
                            stage: "execute".into(),
                            member: MemberId::new(),
                            role: WorkflowMemberRole::Worker,
                            iteration: 0,
                            step: 0,
                            candidate_index: 0,
                            model: ModelRef::new("hya/offline"),
                            reasoning: "none".into(),
                            failure_class: WorkflowRouteFailureClass::None,
                        }],
                    }],
                    error: None,
                },
            },
        };
        assert!(run_succeeded(&run_res));
        let rendered = format_text_result(&run_res);
        for needle in [
            "workflow routed: completed (replayed)",
            "execute [completed] agent=hya-task members=0",
            "selected worker model: #0 hya/offline reasoning=none",
            "route outcome: role=worker iteration=0 step=0 candidate=#0 model=hya/offline reasoning=none class=none",
        ] {
            assert!(rendered.contains(needle), "missing {needle:?}:\n{rendered}");
        }
        let empty_state = format_text_result(&WorkflowCommandResult::State {
            state: WorkflowProjection::default(),
        });
        assert!(
            empty_state.contains("selected workflow: (none)")
                && empty_state.contains("latest run: (none)")
        );
    }

    #[tokio::test]
    async fn read_only_catalog_resolves_project_roots_subdirectories_and_malformed_warnings() {
        let root = std::env::temp_dir().join(format!("hya-wf-catalog-unit-{}", SessionId::new()));
        let (first, second) = (root.join("first"), root.join("second"));
        let nested = first.join("src/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(second.join(".hya/bundles/catalog/workflows")).unwrap();
        std::fs::write(
            second.join(".hya/bundles/catalog/bundle.yaml"),
            "schema_version: 2\nkind: Bundle\nidentity: {id: test/catalog, version: 1.0.0, publisher: test}\nagents: [{id: catalog-worker, role: subagent}]\nworkflow: {id: echo-input, path: workflows/echo-input.hya.md}\n",
        )
        .unwrap();
        std::fs::write(
            second.join(".hya/bundles/catalog/workflows/echo-input.hya.md"),
            ECHO_WF.replace("hya-task", "catalog-worker"),
        )
        .unwrap();
        std::fs::create_dir_all(first.join(".hya/bundles/broken")).unwrap();
        std::fs::write(first.join(".hya/bundles/broken/bundle.yaml"), "kind: [\n").unwrap();

        let db = root.join("sessions.db");
        let store = hya_store::SessionStore::connect(db.to_str().unwrap())
            .await
            .unwrap();
        store
            .create_project(
                "catalog",
                &[
                    first.to_string_lossy().into_owned(),
                    second.to_string_lossy().into_owned(),
                ],
            )
            .await
            .unwrap();
        drop(store);
        let wal = root.join("sessions.db-wal");
        for _ in 0..100 {
            if !wal.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let before = std::fs::read(&db).unwrap();

        let mut warnings = Vec::new();
        let cat = hya_app::WorkflowCatalog::inspect_read_only(db.to_str().unwrap(), &nested, |w| {
            warnings.push(w.to_owned());
        })
        .await
        .unwrap();
        assert_eq!(cat.info("echo-input").unwrap().identity.name, "echo-input");
        assert!(
            warnings.iter().any(|w| w.contains("broken")),
            "{warnings:?}"
        );
        assert_eq!(std::fs::read(&db).unwrap(), before, "read-only DB changed");
        assert!(!std::path::PathBuf::from(format!("{}.runtime-owner.lock", db.display())).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
