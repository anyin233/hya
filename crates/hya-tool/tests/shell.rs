//! Integration tests for the canonical `bash` tool and its hidden `shell` alias.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-bash-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

/// Run one Bash call behind a bounded test guard so a broken timeout path cannot
/// leave the integration suite waiting indefinitely.
async fn execute_with_guard(
    tool: &dyn hya_tool::Tool,
    ctx: &ToolCtx,
    input: Value,
) -> Result<Value, ToolError> {
    tokio::time::timeout(Duration::from_secs(8), tool.execute(ctx, input))
        .await
        .expect("Bash call exceeded the integration-test guard")
}

/// Quote a filesystem path for the POSIX shell command fixtures below.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

/// Assert the structured result envelope and the bounded metadata allowlist.
fn assert_result_shape(result: &Value, command: &str) {
    let object = result.as_object().expect("Bash result must be an object");
    let result_keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(
        result_keys,
        BTreeSet::from(["metadata", "output", "title"]),
        "Bash results must retain their structured envelope"
    );
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
        "timeoutSeconds",
        "truncated",
    ]);
    assert!(
        metadata.keys().all(|key| allowed.contains(key.as_str())),
        "Bash metadata must not expose arbitrary fields: {metadata:?}"
    );
    assert!(metadata["exit"].is_null() || metadata["exit"].is_i64());
    assert!(metadata["timedOut"].is_boolean());
    assert!(metadata["truncated"].is_boolean());
    assert!(metadata["pty"].is_boolean());
    assert!(metadata["cwd"].is_string());
    assert!(metadata["durationMs"].is_number());
    assert!(metadata["timeoutSeconds"].is_number());
    if let Some(clamped) = metadata.get("timeoutClamped") {
        assert!(clamped.is_boolean());
    }
    if let Some(output_path) = metadata.get("outputPath") {
        assert!(output_path.is_string());
    }
}

#[cfg(unix)]
/// Wait until a command publishes a parsed child PID through its marker file.
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
/// Return whether a process still exists without treating permission errors as
/// proof that the process exited.
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) performs an existence check and sends no signal.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
/// Wait for a process-group member to disappear after timeout or cancellation.
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

#[test]
fn bash_schema_is_closed_and_hides_the_shell_alias() {
    // Given
    let registry = ToolRegistry::builtins();

    // When
    let schemas = registry.schemas();
    let advertised_names = schemas
        .iter()
        .map(|schema| schema.name.as_str())
        .collect::<Vec<_>>();
    let tool = registry.get("bash").unwrap();
    let schema = tool.schema();
    let properties = schema.input_schema["properties"].as_object().unwrap();

    // Then
    assert_eq!(schema.name.as_str(), "bash");
    assert!(advertised_names.contains(&"bash"));
    assert!(!advertised_names.contains(&"shell"));
    assert_eq!(
        advertised_names
            .iter()
            .filter(|name| **name == "bash")
            .count(),
        1
    );
    assert!(registry.get("shell").is_some());
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
    assert_eq!(properties["command"]["type"], "string");
    assert_eq!(properties["env"]["type"], "object");
    assert_eq!(properties["env"]["additionalProperties"]["type"], "string");
    assert_eq!(properties["timeout"]["type"], "number");
    assert_eq!(properties["cwd"]["type"], "string");
    assert_eq!(properties["pty"]["type"], "boolean");
    assert!(!properties.contains_key("workdir"));
}

#[tokio::test]
async fn bash_executes_canonical_calls_and_hidden_shell_alias_dispatches() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let registry = ToolRegistry::builtins();
    let canonical = registry.get("bash").unwrap();
    let alias = registry.get("shell").unwrap();

    // When
    let canonical_result = execute_with_guard(
        canonical.as_ref(),
        &ctx,
        json!({ "command": "printf canonical", "timeout": 1.0 }),
    )
    .await
    .unwrap();
    let alias_result = execute_with_guard(
        alias.as_ref(),
        &ctx,
        json!({ "command": "printf alias", "timeout": 1.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        canonical.result_policy(),
        hya_tool::ToolResultPolicy::Coding
    );
    assert_eq!(alias.result_policy(), hya_tool::ToolResultPolicy::Coding);
    assert_result_shape(&canonical_result, "printf canonical");
    assert_result_shape(&alias_result, "printf alias");
    assert_eq!(canonical_result["output"], "canonical");
    assert_eq!(alias_result["output"], "alias");
    assert_eq!(canonical_result["metadata"]["exit"], 0);
    assert_eq!(alias_result["metadata"]["exit"], 0);
}

#[tokio::test]
async fn bash_accepts_env_without_disclosing_values_in_the_result() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let secret = "bash-secret-value-7e4c";
    let command = "test -n \"$HYA_PRIVATE_VALUE\" && printf ready";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({
            "command": command,
            "env": { "HYA_PRIVATE_VALUE": secret },
            "timeout": 1.0
        }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, command);
    assert_eq!(result["output"], "ready");
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(!serialized.contains(secret));
}

#[tokio::test]
async fn bash_rejects_legacy_workdir_and_other_unknown_input_keys() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf never", "workdir": "." }),
    )
    .await;

    // Then
    assert!(matches!(result, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn bash_uses_canonical_cwd_and_records_it_in_metadata() {
    // Given
    let dir = tempdir();
    let subdir = dir.join("subdir");
    tokio::fs::create_dir_all(&subdir).await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "pwd", "timeout": 1.0, "cwd": "subdir" }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, "pwd");
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .trim()
            .ends_with("/subdir")
    );
    assert!(
        result["metadata"]["cwd"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("/subdir")
    );
}

#[tokio::test]
async fn bash_uses_default_and_zero_disabled_timeouts() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let default_result =
        execute_with_guard(tool.as_ref(), &ctx, json!({ "command": "printf default" }))
            .await
            .unwrap();
    let disabled_result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf disabled", "timeout": 0.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&default_result, "printf default");
    assert_result_shape(&disabled_result, "printf disabled");
    assert_eq!(
        default_result["metadata"]["timeoutSeconds"].as_f64(),
        Some(300.0)
    );
    assert_eq!(
        disabled_result["metadata"]["timeoutSeconds"].as_f64(),
        Some(0.0)
    );
    assert_eq!(default_result["metadata"]["timedOut"], false);
    assert_eq!(disabled_result["metadata"]["timedOut"], false);
}

#[tokio::test]
async fn bash_rejects_negative_and_nonfinite_timeout_inputs() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let negative = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf never", "timeout": -1.0 }),
    )
    .await;
    let nonfinite = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf never", "timeout": "NaN" }),
    )
    .await;

    // Then
    assert!(matches!(negative, Err(ToolError::Input(_))));
    assert!(matches!(nonfinite, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn bash_clamps_positive_timeout_bounds_and_reports_a_notice() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let lower = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf lower", "timeout": 0.25 }),
    )
    .await
    .unwrap();
    let upper = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "printf upper", "timeout": 7200.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&lower, "printf lower");
    assert_result_shape(&upper, "printf upper");
    assert_eq!(lower["metadata"]["timeoutSeconds"].as_f64(), Some(1.0));
    assert_eq!(upper["metadata"]["timeoutSeconds"].as_f64(), Some(3600.0));
    assert_eq!(lower["metadata"]["timeoutClamped"], true);
    assert_eq!(upper["metadata"]["timeoutClamped"], true);
    assert!(
        lower["output"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("clamp")
    );
    assert!(
        upper["output"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("clamp")
    );
}

#[tokio::test]
async fn bash_checks_command_permission_before_external_cwd_permission() {
    // Given
    let dir = tempdir();
    let outside = tempdir();
    let marker = dir.join("must-not-run");
    let ctx = ctx_with(
        vec![
            deny(Action::Bash, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir,
    );
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!("printf ran > {}", shell_quote(&marker));

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "cwd": outside.to_string_lossy() }),
    )
    .await;

    // Then
    assert!(matches!(
        result,
        Err(ToolError::Permission(hya_tool::PermissionError::Denied {
            action: Action::Bash,
            ..
        }))
    ));
    assert!(!marker.exists());
}

#[tokio::test]
async fn bash_requires_external_directory_permission_for_outside_cwd() {
    // Given
    let dir = tempdir();
    let outside = tempdir();
    let ctx = ctx_with(
        vec![
            allow(Action::Bash, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir,
    );
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "pwd", "cwd": outside.to_string_lossy() }),
    )
    .await;

    // Then
    assert!(matches!(
        result,
        Err(ToolError::Permission(hya_tool::PermissionError::Denied {
            action: Action::ExternalDirectory,
            ..
        }))
    ));
}

#[tokio::test]
async fn bash_allows_external_cwd_when_external_permission_is_granted() {
    // Given
    let dir = tempdir();
    let outside = tempdir();
    let ctx = ctx_with(
        vec![
            allow(Action::Bash, "*"),
            allow(Action::ExternalDirectory, "*"),
        ],
        dir,
    );
    let tool = ToolRegistry::builtins().get("bash").unwrap();

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": "pwd", "cwd": outside.to_string_lossy() }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, "pwd");
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .contains(outside.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn bash_returns_structured_nonzero_exit_results() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "printf stdout; printf stderr >&2; exit 17";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, command);
    assert_eq!(result["metadata"]["exit"], 17);
    assert_eq!(result["metadata"]["timedOut"], false);
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("stdout"));
    assert!(output.contains("stderr"));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_returns_completed_result_and_cleans_process_group() {
    // Given
    let dir = tempdir();
    let pid_path = dir.join("timeout-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "(while :; do :; done) & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
        shell_quote(&pid_path)
    );
    let task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 1.0 }))
            .await
    });
    let child_pid = wait_for_pid(&pid_path).await;

    // When
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("timed-out Bash call exceeded the integration-test guard")
        .expect("timed-out Bash task panicked")
        .unwrap();

    // Then
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
async fn bash_cancellation_returns_cancelled_and_cleans_process_group() {
    // Given
    let dir = tempdir();
    let pid_path = dir.join("cancel-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let cancel = ctx.cancel.clone();
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "(while :; do :; done) & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
        shell_quote(&pid_path)
    );
    let task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 0.0 }))
            .await
    });
    let child_pid = wait_for_pid(&pid_path).await;

    // When
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled Bash call exceeded the integration-test guard")
        .expect("cancelled Bash task panicked");

    // Then
    assert!(matches!(result, Err(ToolError::Cancelled)));
    wait_for_process_exit(child_pid).await;
}

#[tokio::test]
async fn bash_captures_stdout_and_stderr_with_per_stream_order() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command =
        "printf 'out-1\\n'; printf 'err-1\\n' >&2; printf 'out-2\\n'; printf 'err-2\\n' >&2";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, command);
    let output = result["output"].as_str().unwrap();
    for marker in ["out-1", "out-2", "err-1", "err-2"] {
        assert!(
            output.contains(marker),
            "missing {marker} in combined output"
        );
    }
    assert!(output.find("out-1").unwrap() < output.find("out-2").unwrap());
    assert!(output.find("err-1").unwrap() < output.find("err-2").unwrap());
}

#[cfg(unix)]
#[tokio::test]
async fn bash_keeps_arrival_order_when_stdout_backpressure_precedes_stderr() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let payload_bytes = 128 * 1024;
    let marker = "stderr-after-stdout-unique";
    let command = format!(
        "python3 - <<'PY'\nimport os\npayload = b'O' * {payload_bytes}\nview = memoryview(payload)\nwhile view:\n    view = view[os.write(1, view):]\nos.write(2, b'{marker}\\n')\nPY"
    );

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 10.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, result["title"].as_str().unwrap());
    assert_eq!(result["metadata"]["truncated"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.len() <= 50 * 1024);
    let output_path = result["metadata"]["outputPath"]
        .as_str()
        .expect("overflow must retain a complete artifact path");
    let artifact = tokio::fs::read(output_path).await.unwrap();
    let marker_at = artifact
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .expect("stderr marker missing from complete artifact");
    assert!(marker_at >= payload_bytes);
    assert!(artifact[..marker_at].iter().all(|byte| *byte == b'O'));
    assert_eq!(&artifact[marker_at..], format!("{marker}\n").as_bytes());
}

#[cfg(unix)]
#[tokio::test]
async fn bash_decodes_invalid_utf8_lossily_without_invalid_result_bytes() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "python3 -c 'import os; os.write(1, b\"ok\\xff\\xfe\")'";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, command);
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("ok\u{fffd}\u{fffd}"));
    assert!(serde_json::to_string(&result).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_observes_tty_and_non_pty_does_not_fallback() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "test -t 0 && test -t 1 && printf tty";

    // When
    let pty_result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0, "pty": true }),
    )
    .await
    .unwrap();
    let pipe_result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0, "pty": false }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&pty_result, command);
    assert_result_shape(&pipe_result, command);
    assert_eq!(pty_result["metadata"]["pty"], true);
    assert_eq!(pipe_result["metadata"]["pty"], false);
    assert_eq!(pty_result["metadata"]["exit"], 0);
    assert_eq!(pipe_result["metadata"]["exit"], 1);
    assert!(pty_result["output"].as_str().unwrap().contains("tty"));
    assert!(!pipe_result["output"].as_str().unwrap().contains("tty"));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_timeout_returns_structured_result_and_cleans_process_group() {
    // Given
    let dir = tempdir();
    let pid_path = dir.join("pty-timeout-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "printf '%s' \"$$\" > {}; while :; do :; done",
        shell_quote(&pid_path)
    );
    let task = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({ "command": command, "timeout": 1.0, "pty": true }),
        )
        .await
    });
    let pid = wait_for_pid(&pid_path).await;

    // When
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("timed-out PTY Bash call exceeded the integration-test guard")
        .expect("timed-out PTY Bash task panicked")
        .unwrap();

    // Then
    assert_result_shape(&result, result["title"].as_str().unwrap());
    assert_eq!(result["metadata"]["pty"], true);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["exit"], Value::Null);
    wait_for_process_exit(pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_cancellation_returns_cancelled_and_cleans_process_group() {
    // Given
    let dir = tempdir();
    let pid_path = dir.join("pty-cancel-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let cancel = ctx.cancel.clone();
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "printf '%s' \"$$\" > {}; while :; do :; done",
        shell_quote(&pid_path)
    );
    let task = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({ "command": command, "timeout": 0.0, "pty": true }),
        )
        .await
    });
    let pid = wait_for_pid(&pid_path).await;

    // When
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled PTY Bash call exceeded the integration-test guard")
        .expect("cancelled PTY Bash task panicked");

    // Then
    assert!(matches!(result, Err(ToolError::Cancelled)));
    wait_for_process_exit(pid).await;
}

#[tokio::test]
async fn bash_uses_bash_language_semantics() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "[[ alpha == a* ]] && printf bash-language";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 1.0 }),
    )
    .await
    .unwrap();

    // Then
    assert_result_shape(&result, command);
    assert_eq!(result["metadata"]["exit"], 0);
    assert_eq!(result["output"], "bash-language");
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_still_applies_after_the_shell_exits_with_open_child_pipes() {
    // Given
    let dir = tempdir();
    let pid_path = dir.join("detached-pipe-child.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "sleep 4 & child=$!; printf '%s' \"$child\" > {}",
        shell_quote(&pid_path)
    );
    let started = std::time::Instant::now();
    let task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 1.0 }))
            .await
    });
    let child_pid = wait_for_pid(&pid_path).await;

    // When
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("Bash call with inherited pipes exceeded the integration-test guard")
        .expect("Bash task with inherited pipes panicked")
        .unwrap();

    // Then
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result["metadata"]["exit"], Value::Null);
    assert_eq!(result["metadata"]["timedOut"], true);
    wait_for_process_exit(child_pid).await;
}
/// Remove a temporary workspace when a test exits, including assertion failures.
struct TempDirGuard(PathBuf);

impl TempDirGuard {
    /// Retain a workspace path for best-effort test cleanup.
    fn new(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// List spill files currently present below a Bash artifact directory.
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

/// Wait until incremental capture creates one spill artifact.
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
/// Kill one test-owned process when an outer guard catches a lifecycle defect.
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
#[tokio::test]
async fn bash_pty_timeout_kills_descendant_after_shell_exits() {
    // Given: the direct PTY shell exits while a background child retains the slave.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let child_pid_path = dir.join("pty-detached-timeout-child.pid");
    let shell_pid_path = dir.join("pty-detached-timeout-shell.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
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
    wait_for_process_exit(shell_pid).await;

    // When: the command deadline expires after the direct shell was reaped.
    let result = match tokio::time::timeout(Duration::from_secs(8), &mut task).await {
        Ok(joined) => joined
            .expect("timed-out PTY Bash task panicked")
            .expect("timed-out PTY Bash call failed unexpectedly"),
        Err(_) => {
            kill_test_process(child_pid);
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
            panic!("PTY timeout waited for a descendant that retained the slave");
        }
    };

    // Then: timeout is structured and the retained process-group member is gone.
    assert_result_shape(&result, result["title"].as_str().unwrap());
    assert_eq!(result["metadata"]["pty"], true);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["exit"], Value::Null);
    wait_for_process_exit(child_pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_pty_cancellation_kills_descendant_after_shell_exits() {
    // Given: the direct PTY shell exits while a background child retains the slave.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let child_pid_path = dir.join("pty-detached-cancel-child.pid");
    let shell_pid_path = dir.join("pty-detached-cancel-shell.pid");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let cancel = ctx.cancel.clone();
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "sh -c 'trap \"\" HUP; printf \"%s\" \"$$\" > \"$1\"; sleep 30' sh {} & while [ ! -s {} ]; do :; done; printf '%s' \"$$\" > {}; exit 0",
        shell_quote(&child_pid_path),
        shell_quote(&child_pid_path),
        shell_quote(&shell_pid_path)
    );
    let mut task = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({ "command": command, "timeout": 0.0, "pty": true }),
        )
        .await
    });
    let child_pid = wait_for_pid(&child_pid_path).await;
    let shell_pid = wait_for_pid(&shell_pid_path).await;
    wait_for_process_exit(shell_pid).await;

    // When: cancellation arrives after the direct shell was reaped.
    cancel.cancel();
    let result = match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
        Ok(joined) => joined
            .expect("cancelled PTY Bash task panicked")
            .expect_err("cancelled PTY Bash call unexpectedly succeeded"),
        Err(_) => {
            kill_test_process(child_pid);
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
            panic!("PTY cancellation waited for a descendant that retained the slave");
        }
    };

    // Then: cancellation is typed and the retained process-group member is gone.
    assert!(matches!(result, ToolError::Cancelled));
    wait_for_process_exit(child_pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bash_spill_artifact_is_private_and_complete() {
    use std::os::unix::fs::PermissionsExt;

    // Given: output crosses the inline bound and must spill to disk.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "python3 -c 'import sys; sys.stdout.write(\"A\" * 51201)'";

    // When
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 5.0 }),
    )
    .await
    .unwrap();

    // Then: the artifact is private and contains every emitted byte.
    assert_result_shape(&result, command);
    assert_eq!(result["metadata"]["truncated"], true);
    let output_path = result["metadata"]["outputPath"]
        .as_str()
        .expect("overflow must retain a complete artifact path");
    let mode = std::fs::metadata(output_path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "Bash spill artifacts must be mode 0600");
    let artifact = tokio::fs::read(output_path).await.unwrap();
    assert_eq!(artifact.len(), 51201);
    assert!(artifact.iter().all(|byte| *byte == b'A'));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_removes_spill_artifact_when_cancelled() {
    // Given: a running command has already created a spill artifact.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let child_pid_path = dir.join("artifact-cancel-child.pid");
    let artifact_root = dir.join(".hya/tool-output");
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let cancel = ctx.cancel.clone();
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = format!(
        "sleep 30 & child=$!; printf '%s' \"$child\" > {}; head -c 51201 /dev/zero; wait \"$child\"",
        shell_quote(&child_pid_path)
    );
    let mut task = tokio::spawn(async move {
        tool.execute(&ctx, json!({ "command": command, "timeout": 0.0 }))
            .await
    });
    let child_pid = wait_for_pid(&child_pid_path).await;
    let artifact = wait_for_artifact(&artifact_root).await;

    // When: cancellation aborts the command after spill creation.
    cancel.cancel();
    let result = match tokio::time::timeout(Duration::from_secs(8), &mut task).await {
        Ok(joined) => joined.expect("cancelled Bash task panicked"),
        Err(_) => {
            kill_test_process(child_pid);
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
            panic!("cancelled Bash call exceeded its outer test guard");
        }
    };

    // Then: a cancelled call leaves no unreachable spill artifact or child.
    assert!(matches!(result, Err(ToolError::Cancelled)));
    wait_for_process_exit(child_pid).await;
    assert!(
        !artifact.exists(),
        "cancelled Bash artifact must be removed"
    );
    assert!(artifact_paths(&artifact_root).await.is_empty());
}

#[tokio::test]
async fn bash_timeout_near_inline_cap_spills_complete_output_after_notices() {
    // Given: raw output fits the sink, but timeout and clamp notices exceed the envelope.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("bash").unwrap();
    let command = "python3 -c 'import sys,time; sys.stdout.write(\"N\" * 51150); sys.stdout.flush(); time.sleep(3)'";

    // When: a clamped deadline terminates the command.
    let result = execute_with_guard(
        tool.as_ref(),
        &ctx,
        json!({ "command": command, "timeout": 0.25 }),
    )
    .await
    .unwrap();

    // Then: notices remain visible, truncation is explicit, and the artifact is complete.
    assert_result_shape(&result, command);
    assert_eq!(result["metadata"]["timedOut"], true);
    assert_eq!(result["metadata"]["timeoutClamped"], true);
    assert_eq!(result["metadata"]["truncated"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.to_ascii_lowercase().contains("clamped"));
    assert!(output.to_ascii_lowercase().contains("timeout"));
    assert!(output.len() <= 50 * 1024);
    let output_path = result["metadata"]["outputPath"]
        .as_str()
        .expect("notices that force truncation must retain an artifact path");
    let artifact = tokio::fs::read(output_path).await.unwrap();
    assert_eq!(artifact.len(), 51150);
    assert!(artifact.iter().all(|byte| *byte == b'N'));
}
