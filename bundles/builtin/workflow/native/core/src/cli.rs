//! `hya workflow` command endpoints (`cli_list`, `cli_info`, `cli_use`,
//! `cli_run`, `cli_state`).
//!
//! Each endpoint takes the generic CLI request body
//! `{"args": {<arg id>: <value>}, "cwd": "<absolute client cwd>"}` and replies
//! with the [`DriverReport`] the command prints. Argument ids: `name`,
//! `revision`, `input` (list), `session`, `json` (flag).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use hya_runtime_abi::{DriverReport, NativeBundleApiReply, NativeBundleApiRequest};
use serde_json::{Value, json};

use crate::error::WorkflowControlError;
use crate::model::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowInfo, WorkflowMemberRole,
    WorkflowModelAssignment, WorkflowModelResolvedCandidate, WorkflowProjection, WorkflowRevision,
    WorkflowRouteFailureClass, WorkflowRunProjection, WorkflowRunResult, WorkflowRunStatus,
    WorkflowStagePlan, WorkflowStageRouteOutcome, WorkflowStageStatus, WorkflowSummary,
};
use crate::service::{WorkflowInvocation, WorkflowService};

/// Run one `cli_*` endpoint and reply its [`DriverReport`] with status 200.
pub(crate) async fn handle(
    service: &Arc<WorkflowService>,
    endpoint: &str,
    request: &NativeBundleApiRequest,
) -> NativeBundleApiReply {
    let report = match run(service, endpoint, request).await {
        Ok(report) => report,
        Err(message) => DriverReport {
            stdout: String::new(),
            stderr: format!("Error: {message}\n"),
            exit_code: 1,
        },
    };
    NativeBundleApiReply {
        status: 200,
        body: json!(report),
    }
}

async fn run(
    service: &Arc<WorkflowService>,
    endpoint: &str,
    request: &NativeBundleApiRequest,
) -> Result<DriverReport, String> {
    let args = &request.body["args"];
    let json = args["json"].as_bool().unwrap_or(false);
    let name = args["name"].as_str().map(str::to_owned);
    let (command, is_run) = match endpoint {
        "cli_list" => {
            let catalog = service.cli_catalog(cwd(request)?).await.map_err(coded)?;
            return finish(
                &WorkflowCommandResult::List {
                    workflows: catalog.summaries(),
                },
                json,
                false,
            );
        }
        "cli_info" => {
            let name = name.ok_or("workflow info requires NAME")?;
            let catalog = service.cli_catalog(cwd(request)?).await.map_err(coded)?;
            let workflow = catalog.describe(&name).map_err(coded)?;
            return finish(&WorkflowCommandResult::Info { workflow }, json, false);
        }
        "cli_use" => (
            WorkflowCommand::Select {
                name: name.ok_or("workflow use requires NAME")?,
                expected_revision: parse_revision(args["revision"].as_str())?,
            },
            false,
        ),
        "cli_run" => {
            let expected_revision = parse_revision(args["revision"].as_str())?;
            let inputs = parse_inputs(&string_list(&args["input"]))?;
            if name.is_none() && args["session"].as_str().is_none() {
                return Err(
                    "workflow run requires NAME unless --session identifies an owning Session"
                        .to_owned(),
                );
            }
            (
                WorkflowCommand::Run {
                    // The `/command` API trims the name and treats an empty one as absent.
                    name: name
                        .map(|name| name.trim().to_owned())
                        .filter(|name| !name.is_empty()),
                    expected_revision,
                    inputs,
                    run: None,
                },
                true,
            )
        }
        "cli_state" => (WorkflowCommand::State, false),
        other => return Err(format!("unknown workflow CLI endpoint `{other}`")),
    };
    let session = request
        .session
        .ok_or_else(|| format!("workflow {endpoint} requires a Session"))?;
    let invocation = WorkflowInvocation {
        caller: None,
        operation: None,
        delivery: WorkflowDelivery::Finished,
    };
    let result = service
        .execute_detached(session, invocation, command)
        .await
        .map_err(|error| {
            let (_, code) = error.http_status_and_v2_code();
            format!("{code}: {error}")
        })?;
    finish(&result, json, is_run)
}

fn cwd(request: &NativeBundleApiRequest) -> Result<&Path, String> {
    let raw = request.body["cwd"].as_str().unwrap_or_default();
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!("cwd must be an absolute path: `{raw}`"));
    }
    Ok(path)
}

fn coded(error: WorkflowControlError) -> String {
    format!("{}: {error}", error.code())
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Render a command result; a run that did not complete exits 1.
fn finish(
    result: &WorkflowCommandResult,
    json: bool,
    is_run: bool,
) -> Result<DriverReport, String> {
    let stdout = if json {
        let mut text = serde_json::to_string_pretty(result)
            .map_err(|error| format!("serialize Workflow result: {error}"))?;
        text.push('\n');
        text
    } else {
        format_text_result(result)
    };
    let mut report = DriverReport {
        stdout,
        stderr: String::new(),
        exit_code: 0,
    };
    if is_run && !run_succeeded(result) {
        report.stderr = match result {
            WorkflowCommandResult::Run { result } => format!(
                "Error: workflow `{}` ended {}\n",
                result.run.workflow.name,
                run_status_name(result.run.status)
            ),
            _ => "Error: Workflow run returned an unexpected result\n".to_owned(),
        };
        report.exit_code = 1;
    }
    Ok(report)
}

fn parse_revision(raw: Option<&str>) -> Result<Option<WorkflowRevision>, String> {
    raw.map(|value| {
        value
            .parse()
            .map_err(|error| format!("invalid Workflow revision `{value}`: {error}"))
    })
    .transpose()
}

fn parse_inputs(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut inputs = BTreeMap::new();
    for pair in raw {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("--input expects KEY=VALUE, got `{pair}`"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err("--input key must not be empty".to_owned());
        }
        if inputs.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("duplicate --input key `{key}`"));
        }
    }
    Ok(inputs)
}

fn format_text_result(result: &WorkflowCommandResult) -> String {
    let mut out = String::new();
    match result {
        WorkflowCommandResult::List { workflows } => write_list(&mut out, workflows),
        WorkflowCommandResult::Info { workflow } => write_info(&mut out, workflow),
        WorkflowCommandResult::Selected { state } | WorkflowCommandResult::State { state } => {
            write_state(&mut out, state);
        }
        WorkflowCommandResult::Run { result } => write_run(&mut out, result),
    }
    out
}

fn write_list(out: &mut String, workflows: &[WorkflowSummary]) {
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

const fn run_status_name(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

const fn stage_status_name(status: WorkflowStageStatus) -> &'static str {
    match status {
        WorkflowStageStatus::Pending => "pending",
        WorkflowStageStatus::Running => "running",
        WorkflowStageStatus::Completed => "completed",
        WorkflowStageStatus::Failed => "failed",
        WorkflowStageStatus::Cancelled => "cancelled",
        WorkflowStageStatus::Skipped => "skipped",
    }
}

const fn workflow_member_role_name(role: WorkflowMemberRole) -> &'static str {
    match role {
        WorkflowMemberRole::Worker => "worker",
        WorkflowMemberRole::Verifier => "verifier",
    }
}

const fn route_failure_name(class: WorkflowRouteFailureClass) -> &'static str {
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
    use hya_proto::{AgentName, MemberId, ModelRef, OwnerRunId, SessionId};

    use super::*;
    use crate::model::{
        WorkflowIdentity, WorkflowModelCandidate, WorkflowRunId, WorkflowSourceId,
        WorkflowStageInfo, WorkflowStageProjection,
    };

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
            assert!(parse_inputs(&raw).unwrap_err().contains(needle));
        }
        assert!(parse_revision(None).unwrap().is_none());
        let hex = "a".repeat(64);
        assert_eq!(
            parse_revision(Some(&hex)).unwrap().unwrap().to_string(),
            hex
        );
        assert!(parse_revision(Some("not-hex")).is_err());
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
        let mut failed = run_res.clone();
        if let WorkflowCommandResult::Run { result } = &mut failed {
            result.run.status = WorkflowRunStatus::Failed;
        }
        let report = finish(&failed, false, true).unwrap();
        assert!(
            report
                .stdout
                .starts_with("workflow routed: failed (replayed)\n")
        );
        assert_eq!(
            (report.stderr.as_str(), report.exit_code),
            ("Error: workflow `routed` ended failed\n", 1)
        );
        let empty_state = format_text_result(&WorkflowCommandResult::State {
            state: WorkflowProjection::default(),
        });
        assert!(
            empty_state.contains("selected workflow: (none)")
                && empty_state.contains("latest run: (none)")
        );
    }
}
