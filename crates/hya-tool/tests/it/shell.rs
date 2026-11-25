//! Integration tests for the canonical `bash` tool and its hidden `shell` alias.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hya_tool::{Action, Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

use crate::common::{TempDirGuard, allow, ctx_with, deny, tempdir};

async fn execute_with_guard(
    tool: &dyn Tool,
    ctx: &ToolCtx,
    input: Value,
) -> Result<Value, ToolError> {
    tokio::time::timeout(Duration::from_secs(8), tool.execute(ctx, input))
        .await
        .expect("Bash call exceeded the integration-test guard")
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn assert_result_shape(result: &Value, command: &str) {
    let object = result.as_object().expect("Bash result must be an object");
    let result_keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(result_keys, BTreeSet::from(["metadata", "output", "title"]));
    assert_eq!(result["title"], command);
    assert!(result["output"].is_string());

    let metadata = result["metadata"]
        .as_object()
        .expect("Bash metadata must be an object");
    let allowed = BTreeSet::from([
        "cwd",
        "durationMs",
        "exit",
        "outputPath",
        "pty",
        "timedOut",
        "timeoutClamped",
        "deadlineCapped",
        "stillRunning",
        "pgid",
        "exitPath",
        "timeoutSeconds",
        "truncated",
    ]);
    assert!(
        metadata.keys().all(|key| allowed.contains(key.as_str())),
        "unexpected Bash metadata keys: {metadata:?}"
    );
    assert!(metadata["exit"].is_null() || metadata["exit"].is_i64());
    assert!(metadata["timedOut"].is_boolean());
    assert!(metadata["truncated"].is_boolean());
    assert!(metadata["pty"].is_boolean());
    assert!(metadata["cwd"].is_string());
    assert!(metadata["durationMs"].is_number());
    assert!(metadata["timeoutSeconds"].is_number());
}

#[cfg(unix)]
async fn wait_for_pid(path: &Path) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(raw) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = raw.trim().parse::<i32>()
                && pid > 1
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Bash child did not publish its PID")
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) probes process existence without sending a signal.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn wait_for_process_exit(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !process_is_alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Bash process-group member survived cleanup guard");
}

#[cfg(unix)]
async fn wait_for_leader_exit(leader: i32) {
    #[cfg(target_os = "linux")]
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{leader}/stat"))
                .expect("leader must remain waitable while its descendant lives");
            if stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with("Z ")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    #[cfg(not(target_os = "linux"))]
    wait_for_process_exit(leader).await;
}

async fn artifact_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        paths.push(entry.path());
    }
    paths
}

async fn wait_for_artifact(root: &Path) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(path) = artifact_paths(root).await.into_iter().next() {
                return path;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Bash did not create its spill artifact")
}

#[cfg(unix)]
fn kill_test_process(pid: i32) {
    // SAFETY: the PID came from a child marker written by this test.
    let result = unsafe { libc::kill(pid, libc::SIGKILL) };
    if result < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "failed to clean test child {pid}"
        );
    }
}

#[cfg(unix)]
struct DaemonCleanup {
    shell_pid: PathBuf,
    daemon_pid: PathBuf,
}

#[cfg(unix)]
impl DaemonCleanup {
    fn kill(&self) {
        for (path, group) in [(&self.shell_pid, true), (&self.daemon_pid, false)] {
            if let Ok(raw) = std::fs::read_to_string(path) {
                let pid = raw.trim().parse::<i32>().unwrap();
                assert!(pid > 1);
                kill_test_process(if group { -pid } else { pid });
            }
        }
    }
}

#[cfg(unix)]
impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
async fn check_setsid_capture(pty: bool, timeout: bool, cancel: bool, output: &str) {
    let dir = tempdir("bash-setsid");
    let _workspace = TempDirGuard::new(&dir);
    let cleanup = DaemonCleanup {
        shell_pid: dir.join("shell.pid"),
        daemon_pid: dir.join("daemon.pid"),
    };
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let cancellation = ctx.cancel.clone();
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let payload_size = 512 * 1024;
    let command = format!(
        "printf '%s' \"$$\" > {}; python3 - {} {} {} {} <<'PY'\nimport os, signal, sys, time\nsignal.signal(signal.SIGHUP, signal.SIG_IGN)\nr, w = os.pipe()\nif os.fork() == 0:\n    os.close(r)\n    os.setsid()\n    with open(sys.argv[1], 'w') as f:\n        f.write(str(os.getpid()))\n    os.write(w, b'ready')\n    os.close(w)\n    if sys.argv[3] == 'chatty':\n        while True:\n            os.write(1, b'daemon-tick\\n')\n            time.sleep(0.03)\n    time.sleep(60)\n    os._exit(0)\nos.close(w)\nos.read(r, 5)\nos.close(r)\nif sys.argv[3] == 'payload':\n    n = int(sys.argv[4])\n    for fd in (1, 2):\n        view = memoryview(b'X' * n)\n        while view:\n            view = view[os.write(fd, view):]\nif sys.argv[2] == 'wait':\n    time.sleep(60)\nPY",
        shell_quote(&cleanup.shell_pid),
        shell_quote(&cleanup.daemon_pid),
        if timeout { "wait" } else { "exit" },
        output,
        payload_size,
    );
    let started = Instant::now();
    let mut task = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "command": command,
                "timeout": if timeout && !cancel { 1 } else { 30 },
                "pty": pty
            }),
        )
        .await
    });
    let daemon_pid = wait_for_pid(&cleanup.daemon_pid).await;
    assert!(process_is_alive(daemon_pid));
    if cancel {
        if !timeout {
            wait_for_process_exit(wait_for_pid(&cleanup.shell_pid).await).await;
        }
        cancellation.cancel();
    }
    let result = tokio::time::timeout(Duration::from_secs(4), &mut task).await;
    let elapsed = started.elapsed();
    cleanup.kill();
    wait_for_process_exit(daemon_pid).await;
    let result = match result {
        Ok(joined) => joined.unwrap(),
        Err(_) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
            panic!("setsid capture exceeded outer guard: {elapsed:?}");
        }
    };
    let bound = if !pty && !timeout && !cancel {
        1500
    } else {
        2500
    };
    assert!(
        elapsed < Duration::from_millis(bound),
        "{elapsed:?} >= {bound}ms"
    );
    if cancel {
        assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
    } else {
        let result = result.unwrap();
        assert_eq!(result["metadata"]["timedOut"], timeout);
        assert_eq!(result["metadata"]["pty"], pty);
        if output == "chatty" {
            assert!(result["output"].as_str().unwrap().contains("daemon-tick"));
        } else if output == "payload" {
            let artifact = tokio::fs::read(result["metadata"]["outputPath"].as_str().unwrap())
                .await
                .unwrap();
            assert_eq!(artifact.len(), 2 * payload_size);
            assert!(artifact.iter().all(|byte| *byte == b'X'));
        }
        assert_eq!(
            result["metadata"]["exit"],
            if timeout { Value::Null } else { json!(0) }
        );
    }
}

#[test]
fn bash_schema_is_closed_and_hides_shell_alias() {
    let registry = hya_tool_loader::builtins();
    let advertised = registry
        .schemas()
        .into_iter()
        .map(|s| s.name.as_str().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        advertised.iter().filter(|n| n.as_str() == "bash").count(),
        1
    );
    assert!(!advertised.iter().any(|n| n == "shell"));
    assert!(registry.get("shell").is_some());

    let schema = registry.get("bash").unwrap().schema();
    let properties = schema.input_schema["properties"].as_object().unwrap();
    assert_eq!(schema.name.as_str(), "bash");
    assert_eq!(schema.input_schema["type"], "object");
    assert_eq!(schema.input_schema["additionalProperties"], false);
    assert_eq!(schema.input_schema["required"], json!(["command"]));
    assert_eq!(
        properties
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["command", "cwd", "env", "pty", "timeout"])
    );
    assert_eq!(properties["env"]["additionalProperties"]["type"], "string");
    assert!(!properties.contains_key("workdir"));
}

#[tokio::test]
async fn bash_execution_alias_env_cwd_semantics_and_nonzero_exit() {
    let dir = tempdir("bash-basic");
    let _guard = TempDirGuard::new(&dir);
    let subdir = dir.join("subdir");
    tokio::fs::create_dir_all(&subdir).await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let registry = hya_tool_loader::builtins();
    let bash = registry.get("bash").unwrap();
    let shell = registry.get("shell").unwrap();
    assert_eq!(bash.result_policy(), hya_tool::ToolResultPolicy::Coding);
    assert_eq!(shell.result_policy(), hya_tool::ToolResultPolicy::Coding);

    let canonical = execute_with_guard(
        bash.as_ref(),
        &ctx,
        json!({ "command": "printf canonical", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    let alias = execute_with_guard(
        shell.as_ref(),
        &ctx,
        json!({ "command": "printf alias", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    assert_result_shape(&canonical, "printf canonical");
    assert_result_shape(&alias, "printf alias");
    assert_eq!(canonical["output"], "canonical");
    assert_eq!(alias["output"], "alias");

    // Env values stay undisclosed; PYTHONUNBUFFERED defaults to "1" unless overridden.
    let secret = "bash-secret-value-7e4c";
    let env_res = execute_with_guard(
        bash.as_ref(),
        &ctx,
        json!({
            "command": "test -n \"$HYA_PRIVATE_VALUE\" && printf '%s:%s' ready \"$PYTHONUNBUFFERED\"",
            "env": { "HYA_PRIVATE_VALUE": secret },
            "timeout": 1.0
        }),
    )
    .await
    .unwrap();
    assert_eq!(env_res["output"], "ready:1");
    assert!(!serde_json::to_string(&env_res).unwrap().contains(secret));

    #[cfg(unix)]
    for pty in [false, true] {
        let overridden = execute_with_guard(
            bash.as_ref(),
            &ctx,
            json!({
                "command": "printf %s \"$PYTHONUNBUFFERED\"",
                "env": { "PYTHONUNBUFFERED": "override" },
                "pty": pty
            }),
        )
        .await
        .unwrap();
        assert_eq!(overridden["output"], "override");
    }

    // Canonical cwd, bash language syntax, and nonzero exit with stdout+stderr.
    let cwd_res = execute_with_guard(
        bash.as_ref(),
        &ctx,
        json!({ "command": "pwd", "cwd": "subdir", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    assert!(
        cwd_res["output"]
            .as_str()
            .unwrap()
            .trim()
            .ends_with("/subdir")
    );
    assert!(
        cwd_res["metadata"]["cwd"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("/subdir")
    );

    let lang = execute_with_guard(
        bash.as_ref(),
        &ctx,
        json!({ "command": "[[ alpha == a* ]] && printf bash-language", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    assert_eq!(lang["output"], "bash-language");

    let nonzero = execute_with_guard(
        bash.as_ref(),
        &ctx,
        json!({ "command": "printf stdout; printf stderr >&2; exit 17", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    assert_eq!(nonzero["metadata"]["exit"], 17);
    assert_eq!(nonzero["metadata"]["timedOut"], false);
    assert!(nonzero["output"].as_str().unwrap().contains("stdout"));
    assert!(nonzero["output"].as_str().unwrap().contains("stderr"));
}

#[tokio::test]
async fn bash_input_validation_timeout_bounds_and_permissions() {
    let dir = tempdir("bash-rules");
    let _guard = TempDirGuard::new(&dir);
    let outside = tempdir("bash-outside");
    let _outside_guard = TempDirGuard::new(&outside);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir.clone());
    let tool = hya_tool_loader::builtins().get("bash").unwrap();

    // Unknown keys and invalid timeout inputs are typed input errors.
    for bad in [
        json!({ "command": "printf never", "workdir": "." }),
        json!({ "command": "printf never", "timeout": -1.0 }),
        json!({ "command": "printf never", "timeout": "NaN" }),
    ] {
        assert!(matches!(
            execute_with_guard(tool.as_ref(), &ctx, bad).await,
            Err(ToolError::Input(_))
        ));
    }

    // Default (300), disabled (0), lower clamp (0.25 -> 1), upper clamp (7200 -> 3600).
    let cases = [
        (json!({ "command": "printf d" }), 300.0, false),
        (json!({ "command": "printf z", "timeout": 0.0 }), 0.0, false),
        (json!({ "command": "printf l", "timeout": 0.25 }), 1.0, true),
        (
            json!({ "command": "printf u", "timeout": 7200.0 }),
            3600.0,
            true,
        ),
    ];
    for (input, expected_secs, clamped) in cases {
        let res = execute_with_guard(tool.as_ref(), &ctx, input)
            .await
            .unwrap();
        assert_eq!(
            res["metadata"]["timeoutSeconds"].as_f64(),
            Some(expected_secs)
        );
        assert_eq!(res["metadata"]["timedOut"], false);
        if clamped {
            assert_eq!(res["metadata"]["timeoutClamped"], true);
            assert!(
                res["output"]
                    .as_str()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("clamp")
            );
        }
    }

    // Denied Action::Bash does not run; allowed Bash ignores ExternalDirectory (ADR-0026).
    let marker = dir.join("must-not-run");
    let denied_ctx = ctx_with(
        vec![
            deny(Action::Bash, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );
    let denied = execute_with_guard(
        tool.as_ref(),
        &denied_ctx,
        json!({ "command": format!("printf ran > {}", shell_quote(&marker)), "cwd": outside }),
    )
    .await;
    assert!(matches!(
        denied,
        Err(ToolError::Permission(hya_tool::PermissionError::Denied {
            action: Action::Bash,
            ..
        }))
    ));
    assert!(!marker.exists());

    let outside_allowed = ctx_with(
        vec![
            allow(Action::Bash, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );
    let pwd = execute_with_guard(
        tool.as_ref(),
        &outside_allowed,
        json!({ "command": "pwd", "cwd": outside }),
    )
    .await
    .unwrap();
    assert!(
        pwd["output"]
            .as_str()
            .unwrap()
            .contains(outside.to_string_lossy().as_ref())
    );

    // Pre-cancelled context rejects before running.
    let cancelled_ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    cancelled_ctx.cancel.cancel();
    assert!(matches!(
        execute_with_guard(
            tool.as_ref(),
            &cancelled_ctx,
            json!({ "command": "echo hi" })
        )
        .await,
        Err(ToolError::Cancelled)
    ));
}

#[tokio::test]
async fn bash_streams_utf8_backpressure_and_private_spill_artifact() {
    let dir = tempdir("bash-io");
    let _guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();

    // Per-stream order.
    let ordered = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({
            "command": "printf 'out-1\\n'; printf 'err-1\\n' >&2; printf 'out-2\\n'; printf 'err-2\\n' >&2",
            "timeout": 1.0
        }),
    )
    .await
    .unwrap();
    let out = ordered["output"].as_str().unwrap();
    assert!(out.find("out-1").unwrap() < out.find("out-2").unwrap());
    assert!(out.find("err-1").unwrap() < out.find("err-2").unwrap());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Invalid UTF-8 decodes lossily.
        let utf8 = execute_with_guard(
            tool.as_ref(),
            &ctx,
            json!({ "command": "python3 -c 'import os; os.write(1, b\"ok\\xff\\xfe\")'", "timeout": 1.0 }),
        )
        .await
        .unwrap();
        assert!(
            utf8["output"]
                .as_str()
                .unwrap()
                .contains("ok\u{fffd}\u{fffd}")
        );

        // 128KB stdout backpressure followed by stderr preserves arrival order and mode 0600 spill.
        let payload_bytes = 128 * 1024;
        let marker = "stderr-after-stdout-unique";
        let command = format!(
            "python3 - <<'PY'\nimport os\npayload = b'O' * {payload_bytes}\nview = memoryview(payload)\nwhile view:\n    view = view[os.write(1, view):]\nos.write(2, b'{marker}\\n')\nPY"
        );
        let spilled = execute_with_guard(
            tool.as_ref(),
            &ctx,
            json!({ "command": command, "timeout": 10.0 }),
        )
        .await
        .unwrap();
        assert_eq!(spilled["metadata"]["truncated"], true);
        assert!(spilled["output"].as_str().unwrap().len() <= 50 * 1024);
        let output_path = spilled["metadata"]["outputPath"].as_str().unwrap();
        assert_eq!(
            std::fs::metadata(output_path).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        let artifact = tokio::fs::read(output_path).await.unwrap();
        let marker_at = artifact
            .windows(marker.len())
            .position(|w| w == marker.as_bytes())
            .expect("stderr marker missing from artifact");
        assert!(marker_at >= payload_bytes);
        assert!(artifact[..marker_at].iter().all(|b| *b == b'O'));
        assert_eq!(&artifact[marker_at..], format!("{marker}\n").as_bytes());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_observes_tty_and_non_pty_does_not() {
    let dir = tempdir("bash-tty");
    let _guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let command = "test -t 0 && test -t 1 && printf tty";

    let pty = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0, "pty": true }),
    )
    .await
    .unwrap();
    let pipe = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0, "pty": false }),
    )
    .await
    .unwrap();
    assert_eq!(pty["metadata"]["pty"], true);
    assert_eq!(pty["metadata"]["exit"], 0);
    assert!(pty["output"].as_str().unwrap().contains("tty"));
    assert_eq!(pipe["metadata"]["pty"], false);
    assert_eq!(pipe["metadata"]["exit"], 1);
    assert!(!pipe["output"].as_str().unwrap().contains("tty"));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_cancellation_cleans_pipes_pty_detached_descendant_and_spill_artifact() {
    let tool = hya_tool_loader::builtins().get("bash").unwrap();

    // 1) Pipes & PTY cancellation clean process groups immediately.
    for pty in [false, true] {
        let dir = tempdir("bash-cancel");
        let _guard = TempDirGuard::new(&dir);
        let pid_path = dir.join("child.pid");
        let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
        let cancel = ctx.cancel.clone();
        let command = if pty {
            format!(
                "printf '%s' \"$$\" > {}; while :; do :; done",
                shell_quote(&pid_path)
            )
        } else {
            format!(
                "(while :; do :; done) & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
                shell_quote(&pid_path)
            )
        };
        let t = tool.clone();
        let task = tokio::spawn(async move {
            t.execute(
                &ctx,
                json!({ "command": command, "timeout": 0.0, "pty": pty }),
            )
            .await
        });
        let pid = wait_for_pid(&pid_path).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(res, Err(ToolError::Cancelled)));
        wait_for_process_exit(pid).await;
    }

    // 2) PTY cancellation after direct shell exits kills retained descendant.
    {
        let dir = tempdir("bash-pty-cancel-detach");
        let _guard = TempDirGuard::new(&dir);
        let child_pid_path = dir.join("child.pid");
        let shell_pid_path = dir.join("shell.pid");
        let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
        let cancel = ctx.cancel.clone();
        let command = format!(
            "sh -c 'trap \"\" HUP; printf \"%s\" \"$$\" > \"$1\"; sleep 30' sh {} & while [ ! -s {} ]; do :; done; printf '%s' \"$$\" > {}; exit 0",
            shell_quote(&child_pid_path),
            shell_quote(&child_pid_path),
            shell_quote(&shell_pid_path)
        );
        let t = tool.clone();
        let mut task = tokio::spawn(async move {
            t.execute(
                &ctx,
                json!({ "command": command, "timeout": 0.0, "pty": true }),
            )
            .await
        });
        let child_pid = wait_for_pid(&child_pid_path).await;
        let shell_pid = wait_for_pid(&shell_pid_path).await;
        wait_for_leader_exit(shell_pid).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(res, Err(ToolError::Cancelled)));
        assert!(!process_is_alive(shell_pid));
        wait_for_process_exit(child_pid).await;
    }

    // 3) Cancelling after spill creation removes the artifact and cleans child.
    {
        let dir = tempdir("bash-cancel-spill");
        let _guard = TempDirGuard::new(&dir);
        let pid_path = dir.join("child.pid");
        let artifact_root = dir.join(".hya/tool-output");
        let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
        let cancel = ctx.cancel.clone();
        let command = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > {}; head -c 51201 /dev/zero; wait \"$child\"",
            shell_quote(&pid_path)
        );
        let t = tool.clone();
        let mut task = tokio::spawn(async move {
            t.execute(&ctx, json!({ "command": command, "timeout": 0.0 }))
                .await
        });
        let child_pid = wait_for_pid(&pid_path).await;
        let artifact = wait_for_artifact(&artifact_root).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(res, Err(ToolError::Cancelled)));
        wait_for_process_exit(child_pid).await;
        assert!(!artifact.exists());
        assert!(artifact_paths(&artifact_root).await.is_empty());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pipe_timeout_cleans_process_group() {
    let dir = tempdir("bash-to-pipe");
    let _guard = TempDirGuard::new(&dir);
    let pid_path = dir.join("timeout-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let command = format!(
        "(while :; do :; done) & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
        shell_quote(&pid_path)
    );
    let task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 1.0 }))
            .await
    });
    let child_pid = wait_for_pid(&pid_path).await;
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_result_shape(&result, result["title"].as_str().unwrap());
    assert_eq!(result["metadata"]["exit"], Value::Null);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["timeoutSeconds"].as_f64(), Some(1.0));
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("timeout")
    );
    wait_for_process_exit(child_pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pipe_timeout_applies_after_shell_exits_with_open_child_pipes() {
    let dir = tempdir("bash-to-open");
    let _guard = TempDirGuard::new(&dir);
    let pid_path = dir.join("detached.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let command = format!(
        "sleep 4 & child=$!; printf '%s' \"$child\" > {}",
        shell_quote(&pid_path)
    );
    let started = Instant::now();
    let task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 1.0 }))
            .await
    });
    let child_pid = wait_for_pid(&pid_path).await;
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result["metadata"]["exit"], Value::Null);
    assert_eq!(result["metadata"]["timedOut"], true);
    wait_for_process_exit(child_pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_timeout_cleans_process_group_and_detached_descendant() {
    let dir = tempdir("bash-to-pty");
    let _guard = TempDirGuard::new(&dir);
    let child_pid_path = dir.join("child.pid");
    let shell_pid_path = dir.join("shell.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let command = format!(
        "sh -c 'trap \"\" HUP; printf \"%s\" \"$$\" > \"$1\"; sleep 30' sh {} & while [ ! -s {} ]; do :; done; printf '%s' \"$$\" > {}; exit 0",
        shell_quote(&child_pid_path),
        shell_quote(&child_pid_path),
        shell_quote(&shell_pid_path)
    );
    let mut task = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({ "command": command, "timeout": 1.0, "pty": true }),
        )
        .await
    });
    let child_pid = wait_for_pid(&child_pid_path).await;
    let shell_pid = wait_for_pid(&shell_pid_path).await;
    wait_for_leader_exit(shell_pid).await;
    let result = tokio::time::timeout(Duration::from_secs(8), &mut task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_result_shape(&result, result["title"].as_str().unwrap());
    assert_eq!(result["metadata"]["pty"], true);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["exit"], Value::Null);
    assert!(!process_is_alive(shell_pid));
    wait_for_process_exit(child_pid).await;
}

#[tokio::test]
async fn bash_timeout_near_inline_cap_spills_complete_output_after_notices() {
    let dir = tempdir("bash-to-cap");
    let _guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let command = "python3 -c 'import sys,time; sys.stdout.write(\"N\" * 51150); sys.stdout.flush(); time.sleep(3)'";

    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 0.25 }),
    )
    .await
    .unwrap();
    assert_result_shape(&result, command);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["timeoutClamped"], true);
    assert_eq!(result["metadata"]["truncated"], true);
    let out = result["output"].as_str().unwrap();
    assert!(out.to_ascii_lowercase().contains("clamped"));
    assert!(out.to_ascii_lowercase().contains("timeout"));
    assert!(out.len() <= 50 * 1024);
    let artifact = tokio::fs::read(result["metadata"]["outputPath"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(artifact.len(), 51150);
    assert!(artifact.iter().all(|b| *b == b'N'));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_same_group_background_output_is_captured() {
    let tool = hya_tool_loader::builtins().get("bash").unwrap();
    let run = |pty: bool| {
        let tool = tool.clone();
        async move {
            let dir = tempdir("bash-bg");
            let _guard = TempDirGuard::new(&dir);
            let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
            let res = execute_with_guard(
                tool.as_ref(),
                &ctx,
                json!({
                    "command": "trap '' HUP; (sleep 0.12; printf 'bg-ok') &",
                    "timeout": 30,
                    "pty": pty
                }),
            )
            .await
            .unwrap();
            assert_eq!(res["output"], "bg-ok");
            assert_eq!(res["metadata"]["exit"], 0);
            assert_eq!(res["metadata"]["timedOut"], false);
        }
    };
    tokio::join!(run(false), run(true));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_setsid_exit_chatty_and_payload_are_bounded() {
    futures::future::join_all([
        check_setsid_capture(false, false, false, "silent"),
        check_setsid_capture(true, false, false, "silent"),
        check_setsid_capture(false, false, false, "chatty"),
        check_setsid_capture(true, false, false, "chatty"),
        check_setsid_capture(false, false, false, "payload"),
        check_setsid_capture(true, false, false, "payload"),
    ])
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_setsid_cancellation_drain_is_bounded() {
    futures::future::join_all([
        check_setsid_capture(false, false, true, "silent"),
        check_setsid_capture(false, true, true, "silent"),
        check_setsid_capture(true, false, true, "silent"),
        check_setsid_capture(true, true, true, "silent"),
    ])
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_setsid_pipe_timeout_drain_is_bounded() {
    check_setsid_capture(false, true, false, "silent").await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_setsid_pty_timeout_drain_is_bounded() {
    check_setsid_capture(true, true, false, "silent").await;
}

/// Uses a thread-scoped deadline with clock advancement so capping and kill run in ~0.35s in-process.
#[cfg(unix)]
#[tokio::test]
async fn deadline_caps_shell_timeouts_across_native_library() {
    let registry = hya_tool_loader::from_tool_families(&["hya/base-tools"]);
    let tool = registry.resolve("bash").unwrap();
    let workdir = tempdir("bash-deadline-cap");
    let _guard = TempDirGuard::new(&workdir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], workdir);

    let scoped = hya_tool::deadline::install_scoped(Duration::from_secs(8));
    for pty in [false, true] {
        for requested in [None, Some(0), Some(300), Some(1)] {
            let mut input = json!({ "command": "printf budget-ok", "pty": pty });
            if let Some(timeout) = requested {
                input["timeout"] = json!(timeout);
            }
            let result = execute_with_guard(tool.tool.as_ref(), &ctx, input)
                .await
                .unwrap();
            assert_result_shape(&result, "printf budget-ok");
            let capped = requested != Some(1);
            assert_eq!(
                result["metadata"]["timeoutSeconds"],
                if capped { 5.0 } else { 1.0 }
            );
            assert_eq!(
                result["metadata"]["deadlineCapped"],
                if capped { json!(true) } else { Value::Null }
            );
            assert_eq!(
                result["output"]
                    .as_str()
                    .unwrap()
                    .contains("Timeout shortened to 5 seconds"),
                capped
            );
            assert!(result["output"].as_str().unwrap().contains("budget-ok"));
        }
    }

    // Install a nested scoped deadline with ~350ms remaining; `end.min(left)` kills `sleep 30` fast.
    let _short = hya_tool::deadline::install_scoped_with_elapsed(
        Duration::from_secs(8),
        Duration::from_millis(7650),
    );
    let stopped = execute_with_guard(
        tool.tool.as_ref(),
        &ctx,
        json!({ "command": "echo $$; exec sleep 30" }),
    )
    .await
    .unwrap();
    assert_eq!(stopped["metadata"]["timedOut"], true);
    let pid: i32 = stopped["output"]
        .as_str()
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .parse()
        .unwrap();
    // SAFETY: signal 0 probes the process without changing it.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    drop(_short);
    drop(scoped);
}

/// Verifies both the `budget/10` default cap and live budgeted yield (`stillRunning` + `exitPath`
/// + `outputPath`) in-process in ~1.2s without a 6s real-time sleep or subprocess re-spawn.
#[cfg(unix)]
#[tokio::test]
async fn budgeted_shell_yields_live_output_and_enforces_tenth_cap() {
    let registry = hya_tool_loader::from_tool_families(&["hya/base-tools"]);
    let tool = registry.resolve("bash").unwrap();
    let workdir = tempdir("bash-yield");
    let _guard = TempDirGuard::new(&workdir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], workdir);

    let _scoped = hya_tool::deadline::install_scoped(Duration::from_secs(50));

    // Default wait is capped to budget/10 (= 5.0s for a 50s budget).
    let tenth = execute_with_guard(
        tool.tool.as_ref(),
        &ctx,
        json!({ "command": "printf tenth-ok" }),
    )
    .await
    .unwrap();
    assert_eq!(tenth["metadata"]["timeoutSeconds"], 5.0);
    assert_eq!(tenth["metadata"]["timedOut"], false);

    // Explicit 1.0s wait yields live output after 1s while the child finishes 150ms later.
    let command = "echo $PYTHONUNBUFFERED; sleep 1.15; head -c 131072 /dev/zero; exit 7";
    let result = execute_with_guard(
        tool.tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0 }),
    )
    .await
    .unwrap();
    assert_result_shape(&result, command);
    let meta = &result["metadata"];
    assert_eq!(meta["stillRunning"], true);
    assert_eq!(meta["timedOut"], false);
    assert_eq!(meta["timeoutSeconds"], 1.0);
    assert!(meta["exit"].is_null());
    assert!(meta["pgid"].is_u64());
    let preview = result["output"].as_str().unwrap();
    assert!(preview.ends_with("1\n"), "unexpected preview: {preview:?}");

    let exit = Path::new(meta["exitPath"].as_str().unwrap());
    tokio::time::timeout(Duration::from_secs(4), async {
        while !exit.exists() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(exit).unwrap(), "7\n");
    assert_eq!(
        std::fs::metadata(meta["outputPath"].as_str().unwrap())
            .unwrap()
            .len(),
        131074
    );
    std::fs::remove_dir_all(exit.parent().unwrap()).unwrap();
}

#[cfg(unix)]
async fn assert_shutdown_drains_suspended_call(
    tool: &dyn Tool,
    pty: bool,
    exits_first: bool,
    scoped: Option<&hya_tool::deadline::RunDeadline>,
) {
    use hya_tool::deadline;

    let (shutdown_tok, hold) = deadline::register_job(scoped).unwrap();
    let dir = tempdir("bash-shutdown");
    let _dir_guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir.clone());
    let cleanup = DaemonCleanup {
        shell_pid: dir.join("leader"),
        daemon_pid: dir.join("descendant"),
    };

    let natural = execute_with_guard(
        tool,
        &ctx,
        json!({
            "command": "trap '' HUP; (exec 0<&- 1>&- 2>&-; sleep 0.08; touch completed) & exit 0",
            "pty": pty,
        }),
    )
    .await
    .unwrap();
    assert_eq!(natural["metadata"]["exit"], 0);
    assert!(dir.join("completed").exists());

    let command = if exits_first {
        "trap '' HUP; echo $$ > leader; (sleep 60 0<&- 1>&- 2>&-) & echo $! > descendant; exit 0"
    } else {
        "echo $$ > leader; sleep 60 & echo $! > descendant; wait"
    };
    let mut call = tool.execute(&ctx, json!({ "command": command, "pty": pty }));
    tokio::select! {
        result = &mut call => panic!("shell finished before shutdown: {result:?}"),
        _ = wait_for_pid(&cleanup.daemon_pid) => {}
    }
    let leader = wait_for_pid(&cleanup.shell_pid).await;
    let descendant = wait_for_pid(&cleanup.daemon_pid).await;
    if exits_first {
        wait_for_leader_exit(leader).await;
        assert!(futures::poll!(&mut call).is_pending());
    }

    let drain = async {
        match scoped {
            Some(d) => d.shutdown_background_jobs().await,
            None => deadline::shutdown_background_jobs().await,
        }
    };
    let mut first = Box::pin(drain);
    assert!(futures::poll!(&mut first).is_pending());
    drop(first);
    assert!(shutdown_tok.is_cancelled());
    drop(hold);

    let resume = async {
        match scoped {
            Some(d) => d.shutdown_background_jobs().await,
            None => deadline::shutdown_background_jobs().await,
        }
    };
    tokio::time::timeout(Duration::from_secs(5), resume)
        .await
        .expect("resumed shutdown");
    assert!(!process_is_alive(leader), "leader was not reaped");
    if process_is_alive(descendant) {
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
                Ok(stat) => assert!(
                    stat.rsplit_once(')').is_some_and(|(_, fields)| matches!(
                        fields.split_whitespace().next(),
                        Some("Z" | "X" | "x")
                    )),
                    "live descendant after shutdown: {stat}"
                ),
                Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
            }
        }
        #[cfg(not(target_os = "linux"))]
        panic!("live descendant after shutdown");
    }

    let late = execute_with_guard(
        tool,
        &ctx,
        json!({ "command": "true", "pty": pty, "cwd": "nonexistent" }),
    )
    .await;
    assert!(matches!(late, Err(ToolError::Cancelled)), "{late:?}");
    assert!(matches!(call.await, Err(ToolError::Cancelled)));
}

#[cfg(unix)]
#[tokio::test]
async fn budgeted_pty_shutdown_drains_suspended_calls_in_process() {
    let tool = hya_tool_loader::load("hya/base-tools")
        .unwrap()
        .tools
        .into_iter()
        .find(|t| t.name() == "bash")
        .unwrap();
    for exits_first in [true, false] {
        let scoped = hya_tool::deadline::install_scoped(Duration::from_secs(180));
        assert_shutdown_drains_suspended_call(tool.as_ref(), true, exits_first, Some(&scoped))
            .await;
    }
}

/// Unbudgeted shutdown closes the process-wide `unbudgeted` registry, so the four
/// `(pipes|pty) × (exits_first)` cases run in parallel subprocesses (~0.3s total).
#[cfg(unix)]
#[test]
fn host_shutdown_drains_suspended_unbudgeted_native_bash_calls() {
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for mode in ["pipes", "pty"] {
        for exits_first in ["yes", "no"] {
            let child = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "shell::native_bash_shutdown_child",
                    "--nocapture",
                ])
                .env("HYA_BASH_SHUTDOWN_TEST", mode)
                .env("HYA_BASH_LEADER_EXITS_FIRST", exits_first)
                .spawn()
                .unwrap();
            children.push((mode, exits_first, child));
        }
    }
    for (mode, exits_first, mut child) in children {
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "{mode}, leader exits first = {exits_first}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn native_bash_shutdown_child() {
    let Ok(mode) = std::env::var("HYA_BASH_SHUTDOWN_TEST") else {
        return;
    };
    let pty = mode != "pipes";
    let exits_first = std::env::var("HYA_BASH_LEADER_EXITS_FIRST").unwrap() == "yes";
    let tool = hya_tool_loader::load("hya/base-tools")
        .unwrap()
        .tools
        .into_iter()
        .find(|t| t.name() == "bash")
        .unwrap();
    assert_shutdown_drains_suspended_call(tool.as_ref(), pty, exits_first, None).await;
}
