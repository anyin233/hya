//! T2.38–T2.44, T2.46–T2.49 (exec side), T2.53 — headless `hya exec` golden paths
//! (running directly against `HeadlessEnv` without booting an unused `hya serve`):
//! - live subagent notices, `--include-subagents` JSONL, 2-notice cap, stalled-worker archive,
//! - time-budget opening & single `FINAL` header, grace cutoff, open-todo non-continuation,
//! - announcement continuation, ignored-notice cap, DSML leak & spurious `<summary>` recovery,
//! - single-turn kernel Rewrite step folding (`T2.53`),
//! - yielded bash process-group reaping (full + `--bare`) and large-output spill path.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::process::Output;
use std::time::{Duration, Instant};

use hya_e2e::{E2eEnvBuilder, HeadlessEnv, ScriptStep, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const WORKER: &str = "You are hya-task";
const NOTICE: &str = "HEADLESS RUN —";
const SEED: u64 = 39;

fn worker_handle() -> String {
    let mut rng = hya_proto::handle_naming::SplitMix64::seeded(SEED);
    let leaf = hya_proto::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng);
    format!("main/{leaf}")
}

fn spawn_worker() -> ScriptStep {
    tool_step(
        "task",
        json!({
            "description": "worker",
            "prompt": "do the unit",
            "subagent_type": "hya-task"
        }),
    )
}

fn bash(command: &str) -> ScriptStep {
    tool_step("bash", json!({ "command": command }))
}

fn busy_worker(secs: u32) -> Vec<ScriptStep> {
    vec![
        bash(&format!("sleep {secs}")),
        tool_step(
            "report",
            json!({"result": "WORKER_RESULT_OK", "outcome": "done"}),
        ),
    ]
}

async fn run_exec(env: &HeadlessEnv, db_name: &str, flags: &[&str], prompt: &str) -> Output {
    let mut cmd = env.exec_command(db_name);
    cmd.arg("--json")
        .args(flags)
        .arg(prompt)
        .env("HYA_HANDLE_SEED", SEED.to_string());
    let output = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .expect("exec finished within 60 s")
        .expect("spawn hya exec");
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    output
}

fn route_strings(env: &HeadlessEnv, marker: &str) -> Vec<String> {
    env.fake
        .route_requests(marker)
        .unwrap()
        .expect("route")
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// T2.38, T2.39, T2.49 (exec side) — live subagent notice + wait/report, 2-notice cap,
/// and stalled-worker archival without server-side `TEAM QUIESCED`.
#[tokio::test]
async fn exec_live_subagent_notices_and_stalled_archive() {
    // 1. T2.38: early lead stop while worker runs gets HEADLESS RUN notice; lead waits,
    // worker reports, `--include-subagents` captures child `report`, interactive tools absent.
    {
        let env = E2eEnvBuilder::new()
            .route(WORKER, busy_worker(1))
            .route(
                ROOT,
                vec![
                    spawn_worker(),
                    text_step("Now let me look at the docs structure to add a page:"),
                    tool_step("wait", json!({"timeout_secs": 60})),
                    text_step("FINAL_ANSWER"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");

        let output = run_exec(&env, "t238.db", &["--include-subagents"], "delegate unit").await;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for route in [ROOT, WORKER] {
            for request in env.fake.route_requests(route).unwrap().expect("route") {
                for tool in request["tools"].as_array().expect("tool definitions") {
                    let name = tool["function"]["name"].as_str().expect("tool name");
                    assert!(
                        !["ask_user", "plan_exit"].contains(&name),
                        "{route} offered {name}"
                    );
                }
            }
        }
        let log: Vec<hya_proto::Envelope> = stdout
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .expect("valid JSONL");
        let member = log.iter().find_map(|e| match e.event {
            hya_proto::Event::SessionCreated {
                session,
                parent: Some(_),
                ..
            } => Some(session),
            _ => None,
        });
        assert!(member.is_some_and(|id| log.iter().any(|e| matches!(
            &e.event,
            hya_proto::Event::ToolCallRequested { session, name, .. }
                if *session == id && name.as_str() == "report"
        ))));
        assert_eq!(
            log.iter().map(|e| e.seq).collect::<BTreeSet<_>>().len(),
            log.len()
        );
        assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(0));
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), 4);
        assert!(!reqs[1].contains(NOTICE) && reqs[2].contains(NOTICE));
        assert!(reqs[3].contains("WORKER_RESULT_OK"));
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
        assert!(stdout.contains("FINAL_ANSWER"));
    }

    // 2. T2.39: two ignored notices in a row end the run promptly with worker still running.
    {
        let env = E2eEnvBuilder::new()
            .route(WORKER, busy_worker(30))
            .route(
                ROOT,
                vec![
                    spawn_worker(),
                    text_step("ROOT_STOP_1"),
                    text_step("ROOT_STOP_2"),
                    text_step("ROOT_STOP_3"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");
        let started = Instant::now();
        let _ = run_exec(&env, "t239.db", &[], "delegate unit").await;
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(1));
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), 4);
        assert!(reqs[2].contains(NOTICE) && reqs[3].contains(NOTICE));
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    }

    // 3. T2.49 (exec): idle worker wakes `wait` as stalled, exec issues notice (never TEAM
    // QUIESCED), lead archives by handle, and finishes cleanly.
    {
        let env = E2eEnvBuilder::new()
            .route(WORKER, vec![text_step(""), text_step("WORKER_EXTRA")])
            .route(
                ROOT,
                vec![
                    spawn_worker(),
                    tool_step("wait", json!({"timeout_secs": 60})),
                    text_step("ROOT_STOP"),
                    tool_step(
                        "archive",
                        json!({"target": worker_handle(), "reason": "stalled"}),
                    ),
                    text_step("FINAL_ANSWER"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");
        let output = run_exec(&env, "t249.db", &[], "delegate, then finish").await;
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), 5);
        assert!(reqs[2].contains("stalled"));
        assert!(reqs.iter().all(|r| !r.contains("TEAM QUIESCED")));
        assert_eq!(reqs[3].matches(NOTICE).count(), 1);
        assert!(reqs[4].contains(&format!("Archived `{}`", worker_handle())));
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
        assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(1));
        let events: Vec<hya_proto::Envelope> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    hya_proto::Event::MessageStarted {
                        role: hya_proto::Role::User,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    hya_proto::Event::MessageStarted {
                        role: hya_proto::Role::Assistant,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
    }
}

/// T2.40–T2.44, T2.46–T2.48, T2.53 — headless continuation, time-budget/grace,
/// DSML & spurious `<summary>` recovery, ignored-notice cap, structured error
/// surface, and single-turn kernel Rewrite step folding.
#[tokio::test]
async fn exec_continuation_budget_grace_recovery_and_step_fold() {
    // 1. T2.40, T2.41, T2.42, T2.44: 40s budget names budget in opening, delivers FINAL
    // once across tool results & continuation, quotes announced next step, and exits on
    // plain summary even with an open todo pending.
    {
        let env = E2eEnvBuilder::new()
            .route(
                ROOT,
                vec![
                    tool_step(
                        "todo",
                        json!({"ops": [{"op": "add", "content": "open-item"}]}),
                    ),
                    bash("echo working"),
                    text_step("Now verify the file format:"),
                    bash("echo checked"),
                    text_step("Done. Changed files:\n- a.txt"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");
        let _ = run_exec(&env, "budget.db", &["--time-budget", "40"], "do task").await;
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), 5, "{}", env.diagnostics());
        assert!(reqs[0].contains("HEADLESS RUN — time budget: 40s"));
        assert!(reqs[2].contains("HEADLESS RUN — FINAL"));
        assert!(reqs[3].contains("Now verify the file format:"));
        assert_eq!(
            reqs[3].matches("HEADLESS RUN — FINAL").count(),
            1,
            "FINAL deadline header must not repeat on continuation"
        );
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    }

    // 2. T2.46: within grace (30s budget) no continuation turn starts.
    {
        let env = E2eEnvBuilder::new()
            .route(
                ROOT,
                vec![text_step("Now let me:"), text_step("ROOT_EXTRA")],
            )
            .build_headless()
            .await
            .expect("headless env");
        let _ = run_exec(&env, "grace.db", &["--time-budget", "30"], "do task").await;
        assert_eq!(route_strings(&env, ROOT).len(), 1);
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    }

    // 3. T2.47 + T2.48 + unknown/malformed tool surface: DSML leak recovery, spurious
    // `<summary>` recovery, and structured ToolError handling in one exec run.
    {
        let env = E2eEnvBuilder::new()
            .route(
                ROOT,
                vec![
                    text_step("<｜DSML｜ invoke>bash\n<parameter name=\"command\">echo recovered > recovered.txt</parameter>\n</｜DSML｜ invoke>"),
                    bash("echo recovered > recovered.txt"),
                    text_step("<analysis>Work remains.</analysis><summary>Check the output.</summary>"),
                    tool_step("no_such_tool", json!({})),
                    tool_step("read", json!({})),
                    text_step("FINAL_ANSWER"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");
        let output = run_exec(&env, "recovery.db", &[], "do task").await;
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), 6, "{}", env.diagnostics());
        assert!(!reqs[0].contains(NOTICE));
        assert!(
            reqs[1].contains(NOTICE)
                && reqs[1].contains("no tool was executed")
                && reqs[1].contains(
                    "Re-issue the intended tool call now using the tool-calling interface"
                )
        );
        assert!(
            reqs[3].contains("internal <summary> block")
                && reqs[3].contains("deliver your normal final answer")
        );
        assert_eq!(
            env.read_project_file("recovered.txt").unwrap(),
            "recovered\n"
        );
        let events: Vec<hya_proto::Envelope> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    hya_proto::Event::MessageStarted {
                        role: hya_proto::Role::User,
                        ..
                    }
                ))
                .count(),
            1
        );
        let errors: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                hya_proto::Event::ToolError {
                    message_text,
                    value,
                    ..
                } => Some((message_text, value)),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].0, "unknown tool: no_such_tool");
        assert_eq!(errors[1].1.as_ref().unwrap()["error"]["type"], "input");
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    }

    // 4. T2.43: two ignored announcement notices in a row end the run.
    {
        let env = E2eEnvBuilder::new()
            .route(
                ROOT,
                vec![
                    text_step("Let me check:"),
                    text_step("Let me check:"),
                    text_step("Let me check:"),
                    text_step("ROOT_EXTRA"),
                ],
            )
            .build_headless()
            .await
            .expect("headless env");
        let _ = run_exec(&env, "cap.db", &[], "do task").await;
        assert_eq!(route_strings(&env, ROOT).len(), 3);
        assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    }

    // 5. T2.53: single turn with 12 large ~11k-token bash steps triggers kernel Rewrite.
    {
        const STEPS: usize = 12;
        const PROMPT: &str = "do the long task";
        let filler = "lorem ipsum dolor sit amet ".repeat(2_200);
        let mut script: Vec<_> = (0..STEPS)
            .map(|step| bash(&format!("true # STEP_{step}_BEGIN {filler}")))
            .collect();
        script.push(text_step("FINAL_ANSWER"));

        let env = E2eEnvBuilder::new()
            .route(ROOT, script)
            .build_headless()
            .await
            .expect("headless env");
        let config_path = env.xdg_config_home.join("hya/config.yaml");
        let cfg = std::fs::read_to_string(&config_path).unwrap().replacen(
            "    - id: model\n",
            "    - id: model\n      limit:\n        context: 120000\n",
            1,
        );
        std::fs::write(&config_path, cfg).unwrap();

        let mut cmd = env.exec_command("stepfold.db");
        cmd.args(["--json", PROMPT])
            .env("HYA_COMPACTION_KEEP_RECENT", "1");
        let output = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .expect("stepfold finished")
            .expect("spawn stepfold");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
        let reqs = route_strings(&env, ROOT);
        assert_eq!(reqs.len(), STEPS + 1);
        let last = reqs.last().unwrap();
        assert!(last.contains("Earlier context was removed by the harness"));
        assert!(last.contains(PROMPT));
        assert!(last.contains(&format!("STEP_{}_BEGIN", STEPS - 1)));
        assert!(!last.contains("STEP_0_BEGIN"));
    }
}

/// Yielded bash process-group reaping (full + `--bare`) and large bash output spill path.
#[cfg(unix)]
#[tokio::test]
async fn exec_yielded_bash_join_and_spilled_output() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Full mode: spilled 60k output preserves full path AND yielded bash is reaped before exit.
    {
        let env = E2eEnvBuilder::new()
            .route(
                ROOT,
                vec![
                    bash(r"head -c 60000 /dev/zero | tr '\0' x"),
                    tool_step("bash", json!({"command": "exec sleep 60", "timeout": 1})),
                    text_step("Done."),
                ],
            )
            .build_headless()
            .await?;
        let output = run_exec(&env, "yield-full.db", &["--time-budget", "180"], "run bash").await;
        let events: Vec<hya_proto::Envelope> = std::str::from_utf8(&output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let metas: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                hya_proto::Event::ToolResult { output, .. } => Some(&output["metadata"]),
                _ => None,
            })
            .collect();
        assert_eq!(
            metas.len(),
            2,
            "expected spilled + yielded bash tool results"
        );

        assert_eq!(metas[0]["exit"], 0);
        let spilled = std::path::Path::new(metas[0]["outputPath"].as_str().ok_or("outputPath")?);
        assert_eq!(
            spilled.parent(),
            Some(env.project.join(".hya/tool-output").as_path())
        );
        assert_eq!(std::fs::read(spilled)?, vec![b'x'; 60000]);

        assert_eq!(metas[1]["stillRunning"], true);
        let pgid = i32::try_from(metas[1]["pgid"].as_u64().ok_or("pgid")?)?;
        assert_eq!(unsafe { libc::kill(pgid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let exit_path = std::path::Path::new(metas[1]["exitPath"].as_str().ok_or("exitPath")?);
        assert_eq!(std::fs::read_to_string(exit_path)?, "137\n");
        std::fs::remove_dir_all(exit_path.parent().ok_or("job dir")?)?;
    }

    // 2. Bare mode: yielded bash is also reaped before exit.
    {
        let env = E2eEnvBuilder::new()
            .scripts(vec![
                tool_step(
                    "bash",
                    json!({"command": "echo $$ > sleep.pid; exec sleep 60", "timeout": 1}),
                ),
                text_step("Done."),
                text_step("Done."),
            ])
            .build_headless()
            .await?;
        let output = run_exec(
            &env,
            "yield-bare.db",
            &["--bare", "--time-budget", "180"],
            "run bare bash",
        )
        .await;
        let events: Vec<hya_proto::Envelope> = std::str::from_utf8(&output.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let note = events
            .iter()
            .find_map(|e| match &e.event {
                hya_proto::Event::ToolResult { output, .. } => output["still_running"].as_str(),
                _ => None,
            })
            .ok_or("missing bare still_running result")?;
        let exit_path = std::path::Path::new(
            note.split('\'')
                .find(|s| s.ends_with("/exit_code"))
                .ok_or("missing exit path")?,
        );
        assert_eq!(std::fs::read_to_string(exit_path)?, "137\n");
        let pid = env
            .read_project_file("sleep.pid")?
            .trim()
            .parse::<libc::pid_t>()?;
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        std::fs::remove_dir_all(exit_path.parent().ok_or("job dir")?)?;
    }

    Ok(())
}
