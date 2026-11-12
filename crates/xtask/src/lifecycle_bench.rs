//! Real app-wired task lifecycle measurements with an offline scripted provider.
use anyhow::{Context, bail};
use hya_core::{
    AgentSpec, BoundSpawnSender, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor,
    RuntimeRegistry, SessionEngine, SubagentGovernor, SubagentLimits,
};
use hya_proto::{
    AgentName, Event, FinishCause, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef,
    ReportOutcome, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::{
    SessionStore,
    bench::{Counts, snapshot},
};
use hya_tool::{Mode, PermissionPlane, PermissionRules, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

struct Script {
    n: usize,
    calls: Mutex<HashMap<SessionId, usize>>,
    root: Mutex<Option<SessionId>>,
    gate: Semaphore,
    entered: mpsc::UnboundedSender<()>,
    root_ready: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        FakeProvider::scripted(vec![]).capabilities(model)
    }
    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        Some(b"lifecycle-bench-v1".to_vec())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let root = *self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let call = {
            let mut calls = self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let call = calls.entry(session).or_default();
            let value = *call;
            *call += 1;
            value
        };
        let steps = if root == Some(session) {
            if call > 0 {
                self.root_ready.notify_one();
                std::future::pending::<()>().await;
            }
            vec![
                FakeStep::ToolCall {
                    name: "task".into(),
                    input: json!({"tasks": (0..self.n).map(|i| json!({"description": format!("worker {i:02}"), "prompt":"Read input.txt and answer.", "subagent_type":"bench-worker"})).collect::<Vec<_>>()}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else if call % 2 == 0 {
            let _ = self.entered.send(());
            self.gate
                .acquire()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?
                .forget();
            vec![
                FakeStep::ToolCall {
                    name: "read".into(),
                    input: json!({"path":"input.txt"}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![
                FakeStep::Text("Measured ".into()),
                FakeStep::Text("answer.".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        };
        Ok(Box::pin(futures::stream::iter(
            FakeProvider::materialize(&steps, session, message)
                .into_iter()
                .map(Ok),
        )))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Row {
    scenario: String,
    n: usize,
    phase: String,
    #[serde(default)]
    wall_ms: f64,
    counts: BTreeMap<String, u64>,
}
fn row(scenario: &str, n: usize, phase: &str, start: Instant, before: Counts) -> Row {
    let after = snapshot();
    Row {
        scenario: scenario.into(),
        n,
        phase: phase.into(),
        wall_ms: start.elapsed().as_secs_f64() * 1000.,
        counts: BTreeMap::from([
            ("statements".into(), after.statements - before.statements),
            (
                "transactions".into(),
                after.transactions - before.transactions,
            ),
            ("events".into(), after.events - before.events),
            ("bytes".into(), after.bytes - before.bytes),
        ]),
    }
}
fn runtime() -> anyhow::Result<Arc<RuntimeRegistry>> {
    use hya_bundle::{BundleCatalog, BundleSource, SourceFile};
    let packages = [("bench-root", "main", "    can_spawn: [bench-worker]\n"), ("bench-worker", "subagent", "")].into_iter().map(|(id, role, extra)| {
        hya_bundle::prepare_package(BundleSource::new(format!("hya/{id}"), vec![SourceFile::new("bundle.yaml", format!("kind: Bundle\nidentity:\n  id: hya/{id}\n  version: 0.0.0\n  publisher: hya\nagents:\n  - id: {id}\n    role: {role}\n    prompt: prompt.md\n{extra}").into_bytes()), SourceFile::new("prompt.md", b"Complete the task.".to_vec())]))
    }).collect::<Result<Vec<_>, _>>()?;
    let catalog = BundleCatalog::from_verified_catalogs(&packages.iter().collect::<Vec<_>>())?;
    Ok(Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        Arc::new(hya_core::AgentCatalog::new(Arc::new(catalog))?),
    )))
}

fn sample(n: usize) -> anyhow::Result<Vec<Row>> {
    let dir = std::env::temp_dir().join(format!("hya-lifecycle-{}", SessionId::new()));
    std::fs::create_dir(&dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(sample_in(n, &dir));
    drop(runtime);
    std::fs::remove_dir_all(&dir)?;
    result
}
async fn sample_in(n: usize, dir: &std::path::Path) -> anyhow::Result<Vec<Row>> {
    std::fs::write(dir.join("input.txt"), "benchmark input\n")?;
    let store =
        SessionStore::connect(dir.join("events.db").to_str().context("database path")?).await?;
    let (tx, mut entered) = mpsc::unbounded_channel();
    let provider = Arc::new(Script {
        n,
        calls: Mutex::new(HashMap::new()),
        root: Mutex::new(None),
        gate: Semaphore::new(0),
        entered: tx,
        root_ready: tokio::sync::Notify::new(),
    });
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        hya_tool::Rule::new(hya_tool::Action::Task, "*", Mode::Allow),
        hya_tool::Rule::new(hya_tool::Action::Read, "*", Mode::Allow),
    ]));
    let (sender, receiver) = BoundSpawnSender::with_capacity(128);
    let bus = EventBus::new(65536);
    let mut events = bus.subscribe();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime()?,
            permission,
            bus,
        )
        .with_handle_rng(Box::new(hya_core::handle_naming::SplitMix64::seeded(0)))
        .with_spawn_sender(sender)
        .with_governor(SubagentGovernor::new(SubagentLimits {
            max_concurrency: 100,
            ..Default::default()
        })),
    );
    let agent = AgentSpec {
        name: AgentName::new("bench-root"),
        model: ModelRef::new("fake"),
        system_prompt: "Benchmark".into(),
        workdir: dir.into(),
        reasoning: None,
    };
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: dir.to_string_lossy().into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await?;
    *provider
        .root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(root);
    let resident = ResidentSupervisor::start(engine.clone());
    let mut tasks = hya_app::spawn_team_supervisor(
        receiver,
        engine.clone(),
        agent.clone(),
        Arc::new(CategoryRegistry::default()),
        resident.clone(),
    );
    hya_store::bench::warm(engine.store()).await?;
    let cancel = CancellationToken::new();
    let worker_engine = engine.clone();
    let worker_cancel = cancel.clone();
    let mut rows = vec![];
    let before = snapshot();
    let start = Instant::now();
    let lead =
        tokio::spawn(async move { worker_engine.run_turn(root, &agent, worker_cancel).await });
    let measured = async {
        for _ in 0..n {
            entered.recv().await.context("worker entry")?;
        }
        provider.root_ready.notified().await;
        rows.push(row("A", n, "spawn", start, before));
        let mut handles: BTreeMap<String, ()> = BTreeMap::new();
        for scenario in ["A", "B"] {
            if scenario == "B" {
                let before = snapshot();
                let start = Instant::now();
                for handle in handles.keys() {
                    engine
                        .mail_send(
                            root,
                            MailEndpoint::Handle(handle.clone()),
                            MailKind::Message,
                            "Read input.txt and answer.".into(),
                        )
                        .await?;
                }
                for _ in 0..n {
                    entered.recv().await.context("revived worker entry")?;
                }
                rows.push(row(scenario, n, "revive", start, before));
            }
            let before = snapshot();
            let start = Instant::now();
            provider.gate.add_permits(n);
            let mut idle = std::collections::HashSet::new();
            let mut text_chunks = 0;
            let mut assistant_messages = std::collections::HashSet::new();
            while idle.len() < n {
                let envelope = events.recv().await?;
                if let Event::MessageStarted {
                    message,
                    role: hya_proto::Role::Assistant,
                    ..
                } = &envelope.event
                {
                    assistant_messages.insert(*message);
                }
                if matches!(envelope.event, Event::TextDelta { session, message, .. } if session != root && assistant_messages.contains(&message))
                {
                    text_chunks += 1;
                }
                if let Event::ToolError {
                    ref message_text, ..
                } = envelope.event
                {
                    bail!("tool failed: {message_text}");
                }
                if let Event::AgentActivityChanged {
                    handle,
                    status: hya_proto::RosterStatus::Idle,
                    ..
                } = envelope.event
                    && handle != "main"
                {
                    idle.insert(handle.clone());
                    handles.insert(handle, ());
                }
            }
            anyhow::ensure!(
                text_chunks == 2 * n,
                "expected two streamed text chunks per member, got {text_chunks}"
            );
            rows.push(row(scenario, n, "work", start, before));
            let before = snapshot();
            let start = Instant::now();
            for handle in handles.keys() {
                resident
                    .report_and_archive(
                        root,
                        handle,
                        ReportOutcome::Done,
                        "Measured answer.".into(),
                    )
                    .await?;
            }
            rows.push(row(scenario, n, "report+archive", start, before));
        }
        anyhow::Ok(())
    };
    let result = tokio::time::timeout(Duration::from_secs(120), measured).await;
    cancel.cancel();
    let _ = lead.await;
    resident
        .drain(FinishCause::Shutdown, Duration::from_secs(10))
        .await;
    tasks.shutdown().await;
    result??;
    Ok(rows)
}

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut json_output = false;
    let mut baseline = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--check" => baseline = Some(args.next().context("--check requires path")?),
            _ => bail!("usage: lifecycle-bench [--json] [--check baseline.json]"),
        }
    }
    let mut rows = vec![];
    for n in [1, 16, 64] {
        rows.extend(sample(n)?);
    }
    if let Some(path) = baseline {
        let expected: Vec<Row> = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(expected.len() == rows.len(), "baseline row count differs");
        for (a, b) in rows.iter().zip(expected.iter()) {
            check_row(a, b)?;
        }
    }
    if json_output {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("scenario N phase wall_ms statements transactions events bytes");
        for r in rows {
            println!(
                "{} {} {} {:.3} {} {} {} {}",
                r.scenario,
                r.n,
                r.phase,
                r.wall_ms,
                r.counts["statements"],
                r.counts["transactions"],
                r.counts["events"],
                r.counts["bytes"]
            );
        }
    }
    Ok(())
}

fn check_row(actual: &Row, expected: &Row) -> anyhow::Result<()> {
    anyhow::ensure!(
        (actual.n, &actual.scenario, &actual.phase)
            == (expected.n, &expected.scenario, &expected.phase),
        "baseline row identity differs"
    );
    anyhow::ensure!(
        actual.counts.keys().eq(expected.counts.keys()),
        "baseline metric names differ"
    );
    for (metric, value) in &actual.counts {
        let tolerance = match metric.as_str() {
            "statements" => 2,
            "bytes" => 3 * (actual.n as u64 + 1),
            _ => 0,
        };
        anyhow::ensure!(
            value.abs_diff(expected.counts[metric]) <= tolerance,
            "count regression: {} N={} {} {}={} baseline={} tolerance={}",
            actual.scenario,
            actual.n,
            actual.phase,
            metric,
            value,
            expected.counts[metric],
            tolerance
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_gates_counts_with_documented_noise_but_ignores_time() -> anyhow::Result<()> {
        let expected = Row {
            scenario: "A".into(),
            n: 1,
            phase: "work".into(),
            wall_ms: 1.0,
            counts: BTreeMap::from([
                ("bytes".into(), 100),
                ("events".into(), 2),
                ("statements".into(), 10),
                ("transactions".into(), 1),
            ]),
        };
        let mut actual: Row = serde_json::from_value(serde_json::to_value(&expected)?)?;
        actual.wall_ms = 999999.0;
        actual.counts.insert("statements".into(), 12);
        actual.counts.insert("bytes".into(), 106);
        check_row(&actual, &expected)?;
        for (metric, value) in [
            ("bytes", 107),
            ("statements", 13),
            ("events", 3),
            ("transactions", 2),
        ] {
            let mut changed: Row = serde_json::from_value(serde_json::to_value(&expected)?)?;
            changed.counts.insert(metric.into(), value);
            assert!(
                check_row(&changed, &expected).is_err(),
                "{metric} regression must fail"
            );
        }
        Ok(())
    }
}
