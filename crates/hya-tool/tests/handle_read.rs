//! Integration tests for `hya-tool`: `read` over internal resource URLs.
//!
//! These cover the seam where the agent's own namespace meets the workspace.
//! The behaviour that matters in both directions: a handle must resolve through
//! the same presentation code an ordinary file gets, and an ordinary path must
//! not notice that handles exist at all.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::handle::{ArtifactHook, ArtifactMeta, ArtifactPlane, ArtifactStore, HandleError};
use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Where a session's spilled tool output lives, relative to its workdir.
const ARTIFACT_DIR: &str = ".hya/tool-output";

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-handle-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf, artifacts: ArtifactPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Edit, "*", Mode::Allow),
    ]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts,
        agents: Default::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        roots: vec![workdir.clone()],
        workdir,
        cancel: CancellationToken::new(),
    }
}

/// Store `body` as if a tool had spilled it, and return its handle.
fn spill(workdir: &Path, body: &str) -> String {
    let store = ArtifactStore::new(workdir.join(ARTIFACT_DIR));
    let meta = store.store("bash", "text/plain", body.as_bytes()).unwrap();
    format!("artifact://{}", meta.id)
}

/// Model-facing text of a Read result, whatever envelope shape it arrived in.
fn output_text(value: &Value) -> String {
    value
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

async fn read(ctx: &ToolCtx, path: &str) -> Result<Value, hya_tool::ToolError> {
    ToolRegistry::builtins()
        .get("read")
        .unwrap()
        .execute(ctx, json!({ "path": path }))
        .await
}

async fn write(ctx: &ToolCtx, path: &str, content: &str) -> Result<Value, hya_tool::ToolError> {
    ToolRegistry::builtins()
        .get("write")
        .unwrap()
        .execute(ctx, json!({ "path": path, "content": content }))
        .await
}

/// The pointer a truncated tool result leaves behind must lead back to the body.
#[tokio::test]
async fn read_resolves_an_artifact_handle_to_its_body() {
    // Given
    let workdir = tempdir();
    let handle = spill(&workdir, "alpha\nbravo\ncharlie\n");
    let ctx = ctx_with(workdir, ArtifactPlane::default());

    // When
    let result = read(&ctx, &handle).await.unwrap();

    // Then
    let output = output_text(&result);
    assert!(output.contains("alpha"), "{output}");
    assert!(output.contains("charlie"), "{output}");
}

/// A projection slices the body, so a handle can be sampled without paying for
/// the whole thing.
#[tokio::test]
async fn read_applies_a_handle_projection() {
    // Given
    let workdir = tempdir();
    let handle = spill(&workdir, "one\ntwo\nthree\nfour\n");
    let ctx = ctx_with(workdir, ArtifactPlane::default());

    // When
    let result = read(&ctx, &format!("{handle}?head=2")).await.unwrap();

    // Then
    let output = output_text(&result);
    assert!(output.contains("one") && output.contains("two"), "{output}");
    assert!(
        !output.contains("four"),
        "head=2 must not reach the fourth line: {output}"
    );
}

/// Hooks run in registration order, each transforming the previous result, which
/// is what lets independent hooks compose into one retrieval.
#[tokio::test]
async fn read_runs_the_artifact_hook_chain_in_order() {
    struct Replace {
        name: &'static str,
        from: &'static str,
        to: &'static str,
    }

    impl ArtifactHook for Replace {
        fn name(&self) -> &str {
            self.name
        }

        fn post_process(
            &self,
            _meta: &ArtifactMeta,
            body: String,
        ) -> Result<Option<String>, HandleError> {
            Ok(Some(body.replace(self.from, self.to)))
        }
    }

    // Given
    let workdir = tempdir();
    let handle = spill(&workdir, "noise\nkeep\n");
    let plane = ArtifactPlane::new(vec![
        Arc::new(Replace {
            name: "strip-noise",
            from: "noise",
            to: "quiet",
        }),
        Arc::new(Replace {
            name: "shout",
            from: "quiet",
            to: "SILENT",
        }),
    ]);
    let ctx = ctx_with(workdir, plane);

    // When
    let result = read(&ctx, &handle).await.unwrap();

    // Then
    let output = output_text(&result);
    assert!(
        output.contains("SILENT"),
        "the second hook must see the first hook's output: {output}"
    );
    assert!(output.contains("keep"), "{output}");
}

/// A hook must never change what was captured. It runs on retrieval so that a
/// hook which turns out to be wrong has not already destroyed the output.
#[tokio::test]
async fn hooks_do_not_alter_the_stored_artifact() {
    struct Truncates;

    impl ArtifactHook for Truncates {
        fn name(&self) -> &str {
            "truncates"
        }

        fn post_process(
            &self,
            _meta: &ArtifactMeta,
            _body: String,
        ) -> Result<Option<String>, HandleError> {
            Ok(Some("gone".to_string()))
        }
    }

    // Given
    let workdir = tempdir();
    let handle = spill(&workdir, "original body\n");
    let ctx = ctx_with(
        workdir.clone(),
        ArtifactPlane::new(vec![Arc::new(Truncates)]),
    );

    // When
    let result = read(&ctx, &handle).await.unwrap();

    // Then
    assert!(output_text(&result).contains("gone"));
    let id = handle.strip_prefix("artifact://").unwrap();
    let stored = std::fs::read_to_string(workdir.join(ARTIFACT_DIR).join(id)).unwrap();
    assert_eq!(
        stored, "original body\n",
        "the captured bytes must survive any hook"
    );
}

/// The whole point of the constraint the user set: ordinary paths are untouched.
#[tokio::test]
async fn an_ordinary_path_is_unaffected_by_the_handle_namespace() {
    // Given
    let workdir = tempdir();
    std::fs::write(workdir.join("notes.txt"), "plain file\n").unwrap();
    let ctx = ctx_with(workdir, ArtifactPlane::default());

    // When
    let result = read(&ctx, "notes.txt").await.unwrap();

    // Then
    assert!(output_text(&result).contains("plain file"));
}

/// An unknown scheme is an error rather than a path, so a typo surfaces instead
/// of resolving to something surprising.
#[tokio::test]
async fn read_rejects_an_unknown_scheme() {
    // Given
    let ctx = ctx_with(tempdir(), ArtifactPlane::default());

    // When
    let error = read(&ctx, "https://example.com/x").await.unwrap_err();

    // Then
    assert!(
        error.to_string().contains("unknown handle scheme"),
        "{error}"
    );
}

/// A handle naming nothing reports that, rather than reporting a missing file
/// under some path the model never asked for.
#[tokio::test]
async fn read_reports_a_missing_artifact_by_its_handle() {
    // Given
    let ctx = ctx_with(tempdir(), ArtifactPlane::default());

    // When
    let error = read(&ctx, "artifact://does-not-exist").await.unwrap_err();

    // Then
    assert!(
        error.to_string().contains("artifact://does-not-exist"),
        "{error}"
    );
}

/// A scratch payload written by handle must come back by the same handle.
/// Without a writer, `local://` resolves nothing and the agent has no way to
/// park a payload outside the transcript.
#[tokio::test]
async fn write_and_read_round_trip_a_local_payload() {
    let dir = tempdir();
    let ctx = ctx_with(dir.clone(), ArtifactPlane::default());

    write(
        &ctx,
        "local://notes/plan.md",
        "step one\nstep two\nstep three\n",
    )
    .await
    .unwrap();

    let value = read(&ctx, "local://notes/plan.md").await.unwrap();
    assert!(
        output_text(&value).contains("step two"),
        "the payload must resolve through its handle: {}",
        output_text(&value)
    );
    assert!(
        dir.join(".hya/local/notes/plan.md").exists(),
        "a scratch payload lands under the local root, not in the workspace"
    );
}

/// Spilling is only safe because the stored bytes stay authoritative, so the
/// write path must refuse an `artifact://` target instead of editing a capture.
#[tokio::test]
async fn write_refuses_a_read_only_handle() {
    let dir = tempdir();
    let ctx = ctx_with(dir.clone(), ArtifactPlane::default());
    let handle = spill(&dir, "captured output");

    let error = write(&ctx, &handle, "overwritten").await.unwrap_err();
    assert!(
        error.to_string().contains("read-only"),
        "expected a read-only refusal, got: {error}"
    );

    let value = read(&ctx, &handle).await.unwrap();
    assert!(
        output_text(&value).contains("captured output"),
        "the refusal must leave the capture intact"
    );
}
