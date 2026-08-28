use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use tokio_util::sync::CancellationToken;

/// User-authored workflow DAGs, discovered from
/// `<workdir>/.hya/workflows` then `$HOME/.config/hya/workflows`
/// (first name wins). hya ships zero built-in workflows.
#[derive(Subcommand)]
pub(crate) enum WorkflowCommand {
    /// List discovered workflows with their declared stages.
    List,
    /// Show one workflow's stage graph and join contract.
    Info {
        /// Workflow `name` as declared in its definition file.
        name: String,
    },
    /// Execute one user-authored workflow DAG end to end.
    ///
    /// The run is bounded by the same `[subagents]` governor limits as the task
    /// tool; every declared input must be supplied via `--input`.
    Run {
        /// Workflow `name` as declared in its definition file.
        name: String,
        /// Provide one declared input value (`--input key=value`, repeatable).
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
        /// Emit the final run report as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

pub(crate) async fn run(command: WorkflowCommand) -> anyhow::Result<()> {
    match command {
        WorkflowCommand::List => list(),
        WorkflowCommand::Info { name } => info(&name),
        WorkflowCommand::Run { name, inputs, json } => {
            run_workflow_command(&name, &inputs, json).await
        }
    }
}

fn parse_inputs(raw: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut inputs = BTreeMap::new();
    for pair in raw {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(anyhow::anyhow!("--input expects KEY=VALUE, got `{pair}`"));
        };
        if key.trim().is_empty() {
            return Err(anyhow::anyhow!("--input key must not be empty"));
        }
        inputs.insert(key.trim().to_string(), value.to_string());
    }
    Ok(inputs)
}

async fn run_workflow_command(
    name: &str,
    raw_inputs: &[String],
    json_output: bool,
) -> anyhow::Result<()> {
    let workdir = std::env::current_dir().context("resolve current directory")?;
    let def = hya_core::load_workflow_by_name(&workdir, name)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let inputs = parse_inputs(raw_inputs)?;
    for key in def.inputs.keys() {
        anyhow::ensure!(
            inputs.contains_key(key),
            "workflow `{}` requires --input {key}=<value> ({})",
            def.name,
            def.inputs.get(key).map(String::as_str).unwrap_or("")
        );
    }
    for key in inputs.keys() {
        anyhow::ensure!(
            def.inputs.contains_key(key),
            "workflow `{}` does not declare input `{key}`; declared inputs: {}",
            def.name,
            if def.inputs.is_empty() {
                "(none)".to_string()
            } else {
                def.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        );
    }

    crate::first_run_config_bootstrap(false)?;
    let store = hya_store::SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = crate::resolve_runtime(None);
    let agent = crate::agent_with_model(&runtime.model, runtime.reasoning);
    let mut built = crate::build_session_engine(
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
    let _responder = crate::spawn_reject_responder(asks);

    let session = engine
        .create(hya_core::CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .context("create workflow lead session")?;

    // Resolve the run context against this engine exactly like the interactive
    // spawn path does: the binding snapshot authorizes stage agents, and each
    // stage member derives its own roster/resources/sidecar from the engine.
    let binding = engine.bind_runtime(&workdir)?;
    let caller = agent.name.to_string();
    let base_agent = engine.agent_spec_for_binding(&binding, &agent, &caller)?;

    let report = hya_core::run_workflow(
        engine.clone(),
        session,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller,
            base_agent,
            inputs,
        },
        CancellationToken::new(),
    )
    .await;

    let report = match report {
        Ok(report) => report,
        Err(error) => {
            built.shutdown().await.ok();
            return Err(anyhow::anyhow!("workflow `{}` failed: {error}", def.name));
        }
    };

    let payload = serde_json::to_value(&report).context("serialize workflow run report")?;
    if json_output {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", serde_json::to_string_pretty(&payload)?).context("write report")?;
    } else {
        print_text_report(&def.name, &payload);
    }

    built
        .shutdown()
        .await
        .context("shutdown spawn supervisor")?;
    if matches!(report.status, hya_core::WorkflowStatus::Completed) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "workflow `{}` ended {:?}",
            def.name,
            report.status
        ))
    }
}

fn print_text_report(name: &str, report: &serde_json::Value) {
    println!(
        "workflow {name}: {}",
        report["status"].as_str().unwrap_or("?")
    );
    let Some(stages) = report["stages"].as_array() else {
        return;
    };
    for stage in stages {
        println!(
            "  {} [{}] agent={}{}",
            stage["stage"].as_str().unwrap_or("?"),
            stage["status"].as_str().unwrap_or("?"),
            stage["agent"].as_str().unwrap_or("?"),
            stage["session"]
                .as_str()
                .map(|s| format!(" session={s}"))
                .unwrap_or_default(),
        );
        if let Some(output) = stage["output"].as_str()
            && !output.trim().is_empty()
        {
            for line in output.lines() {
                println!("    {line}");
            }
        }
    }
}

fn list() -> anyhow::Result<()> {
    let workdir = std::env::current_dir().context("resolve current directory")?;
    let files = hya_core::discover_workflow_files(&workdir);
    if files.is_empty() {
        println!(
            "no workflows found — author YAML definitions under .hya/workflows/ \
             (see docs/workflows.md)"
        );
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    for path in files {
        let line = match hya_core::load_workflow_file(&path) {
            Ok(def) => format!(
                "{}\n  {} (stages: {})\n",
                def.name,
                path.display(),
                def.stages
                    .iter()
                    .map(|stage| stage.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Err(error) => format!("<invalid> {}\n  {error}\n", path.display()),
        };
        if out.write_all(line.as_bytes()).is_err() {
            break;
        }
    }
    Ok(())
}

fn info(name: &str) -> anyhow::Result<()> {
    let workdir = std::env::current_dir().context("resolve current directory")?;
    let def = hya_core::load_workflow_by_name(&workdir, name)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    println!("{} — {}", def.name, def.description);
    println!(
        "failure policy: {} | inputs: {}",
        def.on_member_failure,
        if def.inputs.is_empty() {
            "(none)".to_string()
        } else {
            def.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    );
    for stage in &def.stages {
        print!("  {}", stage.id);
        if !stage.needs.is_empty() {
            print!(" (after {})", stage.needs.join(", "));
        }
        println!(" <- agent `{}` [{}]", stage.agent, stage.mode);
        if let Some(verify) = &stage.verify {
            println!(
                "      loop verify: agent `{}`, until: {}, max_iterations: {}",
                verify.agent, verify.until, verify.max_iterations
            );
        }
    }
    Ok(())
}
