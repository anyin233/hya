//! A headless `hya exec` run does not end while the lead's own work is
//! unfinished: when its turn ends on its own with a last message announcing
//! a step it did not take, the run gives it another turn with a `HEADLESS RUN`
//! notice. A plain summary ends the run, open todos included (todos take no
//! part in stop decisions); a notice ignored twice in a row ends it too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::Output;
use std::time::Duration;

use hya_e2e::{E2eEnv, E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
/// Every harness notice opens with this; the hya-main prompt names `HEADLESS RUN` bare.
const NOTICE: &str = "HEADLESS RUN —";

/// Run `hya exec --json [extra..] prompt` direct (its own db, so it does not
/// route to the env's `serve`) with the env's config and FakeLlm.
async fn run_exec(env: &E2eEnv, extra: &[&str], prompt: &str) -> Output {
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let run = tokio::process::Command::new(&backend.binary)
        .args(["--yolo", "--model", "fake/model", "--db"])
        .arg(root.join("exec.db"))
        .args(["exec", "--json"])
        .args(extra)
        .arg(prompt)
        .current_dir(&backend.project)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
        .env("XDG_DATA_HOME", &backend.xdg_data_home)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env_remove("HYA_MODEL")
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(90), run)
        .await
        .expect("exec finished within 90 s")
        .expect("spawn hya exec")
}

async fn run_lead(script: Vec<ScriptStep>, extra: &[&str]) -> (E2eEnv, Output, Vec<String>) {
    let env = E2eEnvBuilder::new()
        .route(ROOT, script)
        .build()
        .await
        .expect("e2e env");
    let output = run_exec(&env, extra, "do the task").await;
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    let requests = env
        .fake
        .route_requests(ROOT)
        .unwrap()
        .expect("root route")
        .iter()
        .map(ToString::to_string)
        .collect();
    (env, output, requests)
}

fn bash(command: &str) -> ScriptStep {
    tool_step("bash", json!({ "command": command }))
}

fn todo_status(updates: &[(&str, &str)]) -> ScriptStep {
    let updates: Vec<_> = updates
        .iter()
        .map(|(id, status)| json!({ "op": "status", "id": id, "status": status }))
        .collect();
    tool_step("todo", json!({ "ops": updates }))
}

#[tokio::test]
async fn e1_unfinished_todos_do_not_continue_the_run() {
    let add = |content: &str| json!({ "op": "add", "content": content });
    let (env, output, requests) = run_lead(
        vec![
            tool_step("todo", json!({ "ops": [add("one"), add("two")] })),
            text_step("PARTIAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 2, "{}", env.diagnostics());
    assert!(requests.iter().all(|request| !request.contains(NOTICE)));
    assert!(String::from_utf8_lossy(&output.stdout).contains("PARTIAL_ANSWER"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e2_an_announced_step_continues_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("Now verify the file format:"),
            bash("echo checked"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(requests[1].contains(NOTICE) && requests[1].contains("Now verify the file format:"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e3_a_plain_summary_ends_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            bash("echo done > a.txt"),
            text_step("Done. Changed files:\n- a.txt"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 2, "{}", env.diagnostics());
    assert!(requests.iter().all(|request| !request.contains(NOTICE)));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e4_two_ignored_notices_end_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("Let me check:"),
            text_step("Let me check:"),
            text_step("Let me check:"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(
        requests.len(),
        3,
        "the stop, then two ignored notices; {}",
        env.diagnostics()
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// With a 40 s budget the run starts inside the final window (90 s): the
/// opening names the budget and the first tool result carries the FINAL
/// notice, once.
#[tokio::test]
async fn e5_a_budgeted_run_gets_its_deadline_notice_in_turn() {
    let (env, _, requests) = run_lead(
        vec![
            bash("sleep 1"),
            bash("echo again"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &["--time-budget", "40"],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(
        requests[0].contains("HEADLESS RUN — time budget: 40s"),
        "opening missing: {}",
        requests[0]
    );
    assert!(
        requests[1].contains("HEADLESS RUN — FINAL"),
        "in-turn notice missing: {}",
        requests[1]
    );
    assert_eq!(
        requests[2].matches("HEADLESS RUN — FINAL").count(),
        1,
        "told once per stage"
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e10_continuation_does_not_repeat_the_deadline_header() {
    let (env, _, requests) = run_lead(
        vec![
            tool_step("todo", json!({"ops": [{"op": "add", "content": "finish"}]})),
            bash("echo working"),
            text_step("Now let me finish it:"),
            todo_status(&[("1", "completed")]),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &["--time-budget", "40"],
    )
    .await;
    assert_eq!(requests.len(), 5, "{}", env.diagnostics());
    assert!(requests[2].contains("HEADLESS RUN — FINAL"));
    assert!(requests[3].contains("your turn ended"));
    assert_eq!(
        requests[3].matches("HEADLESS RUN — FINAL").count(),
        1,
        "the tool result already delivered the deadline header"
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// Within the grace (30 s for a 30 s budget) no continuation turn starts.
#[tokio::test]
async fn e7_no_continuation_past_the_grace() {
    let (env, _, requests) = run_lead(
        vec![text_step("Now let me:"), text_step("ROOT_EXTRA")],
        &["--time-budget", "30"],
    )
    .await;
    assert_eq!(requests.len(), 1, "{}", env.diagnostics());
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e9_dsml_leak_triggers_continuation_and_recovers() {
    let (env, output, requests) = run_lead(
        vec![
            text_step("<｜DSML｜ invoke>bash\n<parameter name=\"command\">echo recovered > recovered.txt</parameter>\n</｜DSML｜ invoke>"),
            bash("echo recovered > recovered.txt"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    ).await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(!requests[0].contains(NOTICE));
    assert!(requests[1].contains(NOTICE));
    assert!(requests[1].contains("no tool was executed"));
    assert!(
        requests[1]
            .contains("Re-issue the intended tool call now using the tool-calling interface")
    );
    assert_eq!(
        env.read_project_file("recovered.txt").unwrap(),
        "recovered\n"
    );
    let events: Vec<hya_proto::Envelope> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("canonical exec envelope"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|envelope| matches!(
                envelope.event,
                hya_proto::Event::MessageStarted {
                    role: hya_proto::Role::User,
                    ..
                }
            ))
            .count(),
        1,
        "continuation must not admit another user prompt"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e8_spurious_summary_triggers_a_user_facing_completion() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("<analysis>Work remains.</analysis><summary>Check the output.</summary>"),
            bash("echo checked"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(requests[1].contains("internal <summary> block"));
    assert!(requests[1].contains("deliver your normal final answer"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn headless_tool_surface_preserves_unknown_and_malformed_call_errors() {
    let (env, output, _) = run_lead(
        vec![
            tool_step("no_such_tool", json!({})),
            tool_step("read", json!({})),
            text_step("FINAL_ANSWER"),
        ],
        &[],
    )
    .await;
    let requests = env.fake.route_requests(ROOT).unwrap().expect("root route");
    for request in requests {
        let tools = request["tools"].as_array().expect("tool definitions");
        for tool in tools {
            let name = tool["function"]["name"].as_str().expect("tool name");
            assert!(
                !["ask_user", "plan_exit"].contains(&name),
                "{name} was offered"
            );
        }
    }
    let events = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<hya_proto::Envelope>(line).expect("exec envelope"))
        .collect::<Vec<_>>();
    let errors = events
        .iter()
        .filter_map(|event| match &event.event {
            hya_proto::Event::ToolError {
                message_text,
                value,
                ..
            } => Some((message_text, value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 2, "{events:?}");
    assert_eq!(errors[0].0, "unknown tool: no_such_tool");
    assert_eq!(
        errors[1].1.as_ref().expect("structured error")["error"]["type"],
        "input"
    );
}

#[tokio::test]
async fn completion_check_runs_once_only_after_tool_use_with_time_to_spare() {
    let check = "[hya] Completion check, sent once";
    for (budget, tool, should_check) in [
        ("900", true, true),
        ("180", true, false),
        ("900", false, false),
    ] {
        let mut script = Vec::new();
        if tool {
            script.push(bash("echo verified > checked"));
        }
        script.extend([
            text_step("Done."),
            text_step("Checked."),
            text_step("UNUSED"),
        ]);
        let (env, _, requests) = run_lead(script, &["--time-budget", budget]).await;
        assert_eq!(
            requests.iter().filter(|r| r.contains(check)).count(),
            usize::from(should_check)
        );
        assert_eq!(
            requests.len(),
            1 + usize::from(tool) + usize::from(should_check)
        );
        if tool {
            assert_eq!(
                std::fs::read_to_string(env.backend.project.join("checked"))
                    .as_deref()
                    .ok(),
                Some("verified\n")
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn budgeted_descendant_output_path_reaches_exec_json()
-> Result<(), Box<dyn std::error::Error>> {
    let (_, output, _) = run_lead(vec![
        tool_step("bash", json!({"command": "(for i in {1..200}; do [ -e release ] && break; sleep .05; done; echo bg-ok; touch finished) &", "timeout": 1})),
        bash("touch release; while [ ! -e finished ]; do sleep .05; done"),
        text_step("Done."),
    ], &["--time-budget", "180"]).await;
    let events: Vec<hya_proto::Envelope> = std::str::from_utf8(&output.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let meta = events
        .iter()
        .find_map(|e| match &e.event {
            hya_proto::Event::ToolResult { output, .. }
                if output["metadata"]["stillRunning"] == true =>
            {
                Some(&output["metadata"])
            }
            _ => None,
        })
        .ok_or("missing yielded tool result")?;
    assert!(meta["pgid"].as_u64().is_some());
    let path = std::path::Path::new(
        meta["outputPath"]
            .as_str()
            .ok_or("missing live output path")?,
    );
    assert_eq!(std::fs::read_to_string(path)?, "bg-ok\n");
    std::fs::remove_dir_all(path.parent().ok_or("missing job directory")?)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_joins_yielded_bash_before_exit() -> Result<(), Box<dyn std::error::Error>> {
    let (_env, output, _) = run_lead(
        vec![
            tool_step("bash", json!({"command": "exec sleep 60", "timeout": 1})),
            text_step("Done."),
        ],
        &["--time-budget", "180"],
    )
    .await;
    let meta = bash_metadata(&output)?;
    assert_eq!(meta["stillRunning"], true);
    let pid = i32::try_from(meta["pgid"].as_u64().ok_or("missing pgid")?)?;
    // Signal zero checks whether the exec'd sleep still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let path = std::path::Path::new(meta["exitPath"].as_str().ok_or("missing exitPath")?);
    assert_eq!(std::fs::read_to_string(path)?, "137\n");
    std::fs::remove_dir_all(path.parent().ok_or("missing job directory")?)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn completed_budgeted_bash_preserves_full_output_path()
-> Result<(), Box<dyn std::error::Error>> {
    let (env, output, _) = run_lead(
        vec![
            bash(r"head -c 60000 /dev/zero | tr '\0' x"),
            text_step("Done."),
        ],
        &["--time-budget", "180"],
    )
    .await;
    let meta = bash_metadata(&output)?;
    assert_eq!(meta["exit"], 0);
    let path = std::path::Path::new(meta["outputPath"].as_str().ok_or("missing outputPath")?);
    assert_eq!(
        path.parent(),
        Some(env.backend.project.join(".hya/tool-output").as_path())
    );
    assert_eq!(std::fs::read(path)?, vec![b'x'; 60000]);
    Ok(())
}

fn bash_metadata(output: &Output) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    for line in std::str::from_utf8(&output.stdout)?.lines() {
        let event: hya_proto::Envelope = serde_json::from_str(line)?;
        if let hya_proto::Event::ToolResult { output, .. } = event.event {
            return Ok(output["metadata"].clone());
        }
    }
    Err("missing Bash tool result".into())
}

#[tokio::test]
async fn unknown_tool_cannot_trigger_completion_check() {
    let (_, _, requests) = run_lead(
        vec![
            tool_step("no_such_tool", json!({})),
            text_step("Done."),
            text_step("UNUSED"),
        ],
        &["--time-budget", "900"],
    )
    .await;
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn vetoed_unknown_tool_cannot_trigger_completion_check()
-> Result<(), Box<dyn std::error::Error>> {
    let env = E2eEnvBuilder::new()
        .route(
            ROOT,
            vec![
                tool_step("no_such_tool", json!({})),
                text_step("Done."),
                text_step("UNUSED"),
            ],
        )
        .build()
        .await?;
    let plugin = env.backend.project.join("veto.py");
    std::fs::write(
        &plugin,
        r#"import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    result = {}
    if req['method'] == 'initialize':
        result = {'protocol_version':1,'plugin':{'id':'veto','version':'1.0.0','kind':'rust'},'hooks':[{'name':'tool.execute.before'}],'tools':[]}
    elif req['method'] == 'hook/tool.execute.before':
        result = {'outcome':'veto', 'reason':'fixture veto'}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#,
    )?;
    let config = env.backend.xdg_config_home.join("hya/config.yaml");
    let settings = std::fs::read_to_string(&config)?.replace(
        "plugins: {}",
        &format!(
            "plugins:\n  veto:\n    command: [python3, {}]",
            serde_json::to_string(&plugin)?
        ),
    );
    std::fs::write(config, settings)?;
    let output = run_exec(&env, &["--time-budget", "900"], "do the task").await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?;
    assert!(events.contains("blocked by plugin: fixture veto"));
    assert_eq!(env.fake.route_remaining(ROOT)?, Some(1));
    Ok(())
}

#[tokio::test]
async fn completion_check_resets_ignored_notice_count() {
    let announce = || text_step("Now verify the file format:");
    let (_, _, requests) = run_lead(
        vec![
            bash("true"),
            announce(),
            announce(),
            text_step("Done."),
            announce(),
            announce(),
            text_step("Checked."),
        ],
        &["--time-budget", "900"],
    )
    .await;
    assert_eq!(requests.len(), 7);
}
