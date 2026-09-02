//! Public atomic filesystem contracts for native hashline Edit.
//!
//! These tests deliberately cross only the public `ToolRegistry` and `ToolCtx`
//! seams. Every anchor is copied from a public Read result. Unix-only identity,
//! link, and mode contracts are gated so portable hosts still compile the
//! shared text-preservation checks.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{
    Action, Decision, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane,
    LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane,
    ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Build an allow rule for one tool resource in the test permission plane.
///
/// # Parameters
/// - `action`: Permission action to allow.
/// - `pattern`: Resource glob accepted by the action.
///
/// # Returns
/// An allow-mode rule for the requested action and resource pattern.
fn allow(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Allow)
}

/// Create a deterministic, isolated directory for one atomic contract test.
///
/// # Returns
/// A newly-created directory under the process temporary directory.
fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("hya-hashline-atomic-{}-{id}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// Construct a ToolCtx with Read and Edit permission and no formatter.
//
/// # Parameters
/// - `workdir`: Working directory used by the public tool path resolver.
//
/// # Returns
/// A context whose filesystem tools may read and edit any test path.
fn ctx_with(workdir: PathBuf) -> ToolCtx {
    ctx_with_formatter(workdir, FormatterPlane::default())
}

/// Construct a ToolCtx with Read/Edit permission and a supplied formatter plane.
//
/// # Parameters
/// - `workdir`: Working directory used by the public tool path resolver.
/// - `formatter`: Formatter plane exercised after a successful mutation.
//
/// # Returns
/// A complete context suitable for direct ToolRegistry execution.
fn ctx_with_formatter(workdir: PathBuf, formatter: FormatterPlane) -> ToolCtx {
    ctx_with_formatter_session(workdir, formatter, None, CancellationToken::new())
}

/// Construct a formatter-backed context with explicit session and cancellation state.
//
/// # Parameters
/// - `workdir`: Working directory used by the public tool path resolver.
/// - `formatter`: Formatter plane exercised after a successful mutation.
/// - `session`: Optional session identity used by snapshot and lock contracts.
/// - `cancel`: Cancellation token observed by the public coding tools.
//
/// # Returns
/// A complete context suitable for a session-scoped filesystem operation.
fn ctx_with_formatter_session(
    workdir: PathBuf,
    formatter: FormatterPlane,
    session: Option<SessionId>,
    cancel: CancellationToken,
) -> ToolCtx {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Read, "*"),
        allow(Action::Edit, "*"),
    ]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter,
        agents: Default::default(),
        workdir,
        cancel,
    }
}

/// Remove a test directory after its assertions complete.
///
/// # Parameters
/// - `directory`: Test directory to remove recursively.
///
/// # Returns
/// Nothing; a cleanup failure panics because it would hide filesystem residue.
async fn cleanup(directory: &Path) {
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// Read one hashline anchor from the public Read result for a unique line.
///
/// # Parameters
/// - `registry`: Public builtin registry containing Read.
/// - `ctx`: Read context for the requested workdir.
/// - `path`: Relative or absolute path accepted by Read.
/// - `line_text`: Exact visible line text to identify in Read output.
///
/// # Returns
/// The copied `LINE#HASH` prefix suitable for a strict Edit operation.
async fn read_anchor(
    registry: &ToolRegistry,
    ctx: &ToolCtx,
    path: &str,
    line_text: &str,
) -> String {
    let read = registry
        .get("read")
        .expect("builtin Read must be registered");
    let result = read
        .execute(ctx, json!({"path": path}))
        .await
        .expect("public Read must return the fixture");
    let output = result["output"]
        .as_str()
        .expect("public Read result must contain string output");
    output
        .lines()
        .find_map(|row| {
            let row = row.trim_start();
            let (prefix, content) = row.split_once(':')?;
            let (line_number, hash) = prefix.split_once('#')?;
            if line_number.parse::<usize>().is_ok() && !hash.is_empty() && content == line_text {
                Some(prefix.to_owned())
            } else {
                None
            }
        })
        .expect("public Read output must contain the requested hashline")
}

/// Apply one strict anchored replacement through the public Edit tool.
///
/// # Parameters
/// - `registry`: Public builtin registry containing Edit.
/// - `ctx`: Edit context for the requested workdir.
/// - `path`: Relative or absolute path accepted by Edit.
/// - `anchor`: `LINE#HASH` copied from public Read.
/// - `replacement`: Replacement text for the anchored line.
///
/// # Returns
/// The public Edit result or its typed tool error.
async fn replace_anchor(
    registry: &ToolRegistry,
    ctx: &ToolCtx,
    path: &str,
    anchor: &str,
    replacement: &str,
) -> Result<Value, ToolError> {
    let edit = registry
        .get("edit")
        .expect("builtin Edit must be registered");
    edit.execute(
        ctx,
        json!({
            "path": path,
            "edits": [{"op": "replace", "pos": anchor, "lines": [replacement]}]
        }),
    )
    .await
}

/// Append lines through the public Edit operation without requiring an anchor.
///
/// # Parameters
/// - `registry`: Public builtin registry containing Edit.
/// - `ctx`: Edit context for the requested workdir.
/// - `path`: Relative or absolute path accepted by Edit.
/// - `lines`: Lines to append at the target end.
///
/// # Returns
/// The public Edit result or its typed tool error.
async fn append_lines(
    registry: &ToolRegistry,
    ctx: &ToolCtx,
    path: &str,
    lines: &[&str],
) -> Result<Value, ToolError> {
    let edit = registry
        .get("edit")
        .expect("builtin Edit must be registered");
    edit.execute(
        ctx,
        json!({
            "path": path,
            "edits": [{"op": "append", "lines": lines}]
        }),
    )
    .await
}

/// List regular temporary files left directly in a test workdir.
///
/// # Parameters
/// - `directory`: Workdir whose mutation residue should be inspected.
///
/// # Returns
/// Paths with a `.tmp` extension that remain after a tool failure.
async fn temporary_files(directory: &Path) -> Vec<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory).await.unwrap();
    let mut temporary = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "tmp")
        {
            temporary.push(entry.path());
        }
    }
    temporary.sort();
    temporary
}

/// Formatter that fails after the atomic file mutation has committed.
struct FailingFormatter;

#[async_trait]
impl FormatterProvider for FailingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Err(FormatterError("forced formatter failure".to_owned()))
    }
}
/// One formatter invocation held open by a test-controlled release channel.
struct FormatterCall {
    file: PathBuf,
    release: oneshot::Sender<()>,
}

/// Optional byte transitions performed around one held formatter invocation.
#[derive(Clone, Copy)]
struct FormatterRewrite {
    in_flight: &'static [u8],
    final_bytes: &'static [u8],
}

/// Formatter provider that exposes invocation order and active-count evidence.
struct ChannelFormatter {
    calls: mpsc::UnboundedSender<FormatterCall>,
    active: Arc<AtomicU64>,
    max_active: Arc<AtomicU64>,
    rewrite: Option<FormatterRewrite>,
}

#[async_trait]
impl FormatterProvider for ChannelFormatter {
    /// Report no formatter catalog rows for this channel-controlled provider.
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    /// Hold formatting until the test sends the invocation's release signal.
    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let result = async {
            if let Some(rewrite) = self.rewrite {
                tokio::fs::write(file, rewrite.in_flight)
                    .await
                    .map_err(|error| {
                        FormatterError(format!("in-flight formatter write failed: {error}"))
                    })?;
            }
            let (release, released) = oneshot::channel();
            self.calls
                .send(FormatterCall {
                    file: file.to_path_buf(),
                    release,
                })
                .map_err(|_| FormatterError("formatter control channel closed".to_owned()))?;
            released
                .await
                .map_err(|_| FormatterError("formatter release channel closed".to_owned()))?;
            if let Some(rewrite) = self.rewrite {
                tokio::fs::write(file, rewrite.final_bytes)
                    .await
                    .map_err(|error| {
                        FormatterError(format!("final formatter write failed: {error}"))
                    })?;
            }
            Ok::<(), FormatterError>(())
        }
        .await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        result?;
        Ok(true)
    }
}

/// Create a formatter plane whose calls can be released one by one by a test.
//
/// # Parameters
/// - `rewrite`: Optional bytes written while formatting is held and after release.
//
/// # Returns
/// The formatter plane, its invocation receiver, and an atomic maximum-active counter.
fn channel_formatter(
    rewrite: Option<FormatterRewrite>,
) -> (
    FormatterPlane,
    mpsc::UnboundedReceiver<FormatterCall>,
    Arc<AtomicU64>,
) {
    let (calls, formatter_calls) = mpsc::unbounded_channel();
    let active = Arc::new(AtomicU64::new(0));
    let max_active = Arc::new(AtomicU64::new(0));
    let provider = ChannelFormatter {
        calls,
        active,
        max_active: Arc::clone(&max_active),
        rewrite,
    };
    (
        FormatterPlane::new(Arc::new(provider)),
        formatter_calls,
        max_active,
    )
}

/// Require that no second formatter invocation starts while a prior call is held.
//
/// # Parameters
/// - `calls`: Formatter invocation channel to inspect.
/// - `operation`: Description included in the failure diagnostic.
//
/// # Returns
/// Nothing; the assertion fails if a formatter starts before the held call releases.
async fn assert_no_formatter_call(
    calls: &mut mpsc::UnboundedReceiver<FormatterCall>,
    operation: &str,
) {
    match tokio::time::timeout(Duration::from_secs(1), calls.recv()).await {
        Err(_) => {}
        Ok(Some(call)) => {
            let _ = call.release.send(());
            panic!("{operation} entered the formatter before the prior call released");
        }
        Ok(None) => panic!("{operation} formatter channel closed unexpectedly"),
    }
}

/// Follow a relative file symlink while keeping the link itself intact.
#[cfg(unix)]
#[tokio::test]
async fn edit_follows_relative_symlink_and_preserves_link_identity() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let target = workdir.join("real.txt");
    let link = workdir.join("relative.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    symlink("real.txt", &link).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "relative.txt", "before").await;
    replace_anchor(&registry, &ctx, "relative.txt", &anchor, "after")
        .await
        .expect("relative symlink edit must succeed");

    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("real.txt")
    );
    cleanup(&workdir).await;
}

/// Follow an absolute file symlink while keeping the link itself intact.
#[cfg(unix)]
#[tokio::test]
async fn edit_follows_absolute_symlink_and_preserves_link_identity() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let target = workdir.join("real.txt");
    let link = workdir.join("absolute.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    symlink(&target, &link).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "absolute.txt", "before").await;
    replace_anchor(&registry, &ctx, "absolute.txt", &anchor, "after")
        .await
        .expect("absolute symlink edit must succeed");

    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    cleanup(&workdir).await;
}

/// Follow a symlink in an intermediate directory component before editing.
#[cfg(unix)]
#[tokio::test]
async fn edit_follows_intermediate_symlink_and_preserves_link_identity() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let real_directory = workdir.join("real");
    let linked_directory = workdir.join("linked");
    let target = real_directory.join("target.txt");
    tokio::fs::create_dir_all(&real_directory).await.unwrap();
    tokio::fs::write(&target, "before\n").await.unwrap();
    symlink("real", &linked_directory).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "linked/target.txt", "before").await;
    replace_anchor(&registry, &ctx, "linked/target.txt", &anchor, "after")
        .await
        .expect("intermediate symlink edit must succeed");

    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(
        std::fs::symlink_metadata(&linked_directory)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&linked_directory).unwrap(),
        PathBuf::from("real")
    );
    cleanup(&workdir).await;
}

/// Reject a dangling symlink without creating its missing target.
#[cfg(unix)]
#[tokio::test]
async fn edit_rejects_dangling_symlink_without_mutation() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let link = workdir.join("dangling.txt");
    let missing_target = workdir.join("missing.txt");
    symlink("missing.txt", &link).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let read_result = registry
        .get("read")
        .unwrap()
        .execute(&ctx, json!({"path": "dangling.txt"}))
        .await;
    assert!(
        read_result.is_err(),
        "Read must not silently follow a dangling link"
    );

    let result = append_lines(&registry, &ctx, "dangling.txt", &["created"]).await;
    assert!(result.is_err(), "Edit must reject a dangling link");
    assert!(!missing_target.exists());
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("missing.txt")
    );
    cleanup(&workdir).await;
}

/// Reject a symlink cycle without replacing either link or changing a sentinel.
#[cfg(unix)]
#[tokio::test]
async fn edit_rejects_symlink_cycle_without_mutation() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let first = workdir.join("first");
    let second = workdir.join("second");
    let sentinel = workdir.join("sentinel.txt");
    tokio::fs::write(&sentinel, "untouched\n").await.unwrap();
    symlink("second", &first).unwrap();
    symlink("first", &second).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let result = append_lines(&registry, &ctx, "first", &["new"]).await;
    let message = result
        .expect_err("a symlink cycle must be rejected")
        .to_string()
        .to_ascii_lowercase();
    assert!(
        message.contains("cycle"),
        "cycle error must identify the cycle"
    );
    assert_eq!(
        tokio::fs::read_to_string(&sentinel).await.unwrap(),
        "untouched\n"
    );
    assert!(
        std::fs::symlink_metadata(&first)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(&second)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&first).unwrap(), PathBuf::from("second"));
    assert_eq!(std::fs::read_link(&second).unwrap(), PathBuf::from("first"));
    cleanup(&workdir).await;
}

/// Allow exactly forty symlink hops and reject a forty-first hop.
#[cfg(unix)]
#[tokio::test]
async fn edit_allows_forty_symlink_hops_but_rejects_forty_one() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let target = workdir.join("forty-target.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    for index in (0..40).rev() {
        let link = workdir.join(format!("forty-{index}"));
        let next = if index + 1 == 40 {
            target.clone()
        } else {
            workdir.join(format!("forty-{}", index + 1))
        };
        symlink(next, link).unwrap();
    }

    let over_directory = workdir.join("over");
    tokio::fs::create_dir_all(&over_directory).await.unwrap();
    let over_target = over_directory.join("target.txt");
    tokio::fs::write(&over_target, "untouched\n").await.unwrap();
    for index in (0..=40).rev() {
        let link = over_directory.join(format!("chain-{index}"));
        let next = if index == 40 {
            over_target.clone()
        } else {
            over_directory.join(format!("chain-{}", index + 1))
        };
        symlink(next, link).unwrap();
    }

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "forty-0", "before").await;
    replace_anchor(&registry, &ctx, "forty-0", &anchor, "after")
        .await
        .expect("the forty-hop limit must include the boundary");
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(
        std::fs::symlink_metadata(workdir.join("forty-0"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let result = append_lines(&registry, &ctx, "over/chain-0", &["new"]).await;
    let message = result
        .expect_err("the forty-first symlink hop must be rejected")
        .to_string()
        .to_ascii_lowercase();
    assert!(
        message.contains("40-hop") || (message.contains("hop") && message.contains("symlink")),
        "hop-limit error must identify the bounded symlink traversal"
    );
    assert_eq!(
        tokio::fs::read_to_string(&over_target).await.unwrap(),
        "untouched\n"
    );
    assert!(
        std::fs::symlink_metadata(over_directory.join("chain-0"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    cleanup(&workdir).await;
}

/// Preserve a hard-linked inode while making the linked name observe the edit.
#[cfg(unix)]
#[tokio::test]
async fn edit_preserves_hard_link_inode_and_content() {
    use std::os::unix::fs::MetadataExt;

    let workdir = tempdir();
    let target = workdir.join("target.txt");
    let link = workdir.join("sibling.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    std::fs::hard_link(&target, &link).unwrap();
    let inode_before = std::fs::metadata(&target).unwrap().ino();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "target.txt", "before").await;
    replace_anchor(&registry, &ctx, "target.txt", &anchor, "after")
        .await
        .expect("hard-linked target edit must succeed");

    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode_before);
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert_eq!(tokio::fs::read_to_string(&link).await.unwrap(), "after\n");
    cleanup(&workdir).await;
}

/// Preserve permission bits when replacing an existing file atomically.
#[cfg(unix)]
#[tokio::test]
async fn edit_preserves_existing_mode() {
    use std::os::unix::fs::PermissionsExt;

    let workdir = tempdir();
    let target = workdir.join("mode.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let mut permissions = std::fs::metadata(&target).unwrap().permissions();
    permissions.set_mode(0o640);
    std::fs::set_permissions(&target, permissions).unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "mode.txt", "before").await;
    replace_anchor(&registry, &ctx, "mode.txt", &anchor, "after")
        .await
        .expect("mode-preserving edit must succeed");

    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    cleanup(&workdir).await;
}

/// Preserve one leading UTF-8 BOM and never duplicate it during Edit.
#[tokio::test]
async fn edit_preserves_existing_bom_without_duplication() {
    let workdir = tempdir();
    let target = workdir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFbefore\nbeta\n")
        .await
        .unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "bom.txt", "beta").await;
    replace_anchor(&registry, &ctx, "bom.txt", &anchor, "BETA")
        .await
        .expect("BOM-preserving edit must succeed");

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFbefore\nBETA\n"
    );
    cleanup(&workdir).await;
}

/// Preserve CRLF bytes while matching anchors against normalized LF text.
#[tokio::test]
async fn edit_preserves_crlf_line_endings() {
    let workdir = tempdir();
    let target = workdir.join("crlf.txt");
    tokio::fs::write(&target, b"before\r\nbeta\r\n")
        .await
        .unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "crlf.txt", "beta").await;
    replace_anchor(&registry, &ctx, "crlf.txt", &anchor, "BETA")
        .await
        .expect("CRLF edit must succeed");

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"before\r\nBETA\r\n"
    );
    cleanup(&workdir).await;
}

/// Preserve the first ending style for mixed files and report a warning.
#[tokio::test]
async fn edit_preserves_first_mixed_line_ending_style_and_warns() {
    let workdir = tempdir();
    let target = workdir.join("mixed.txt");
    tokio::fs::write(&target, b"before\r\nbeta\ngamma\rdelta\n")
        .await
        .unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "mixed.txt", "beta").await;
    let result = replace_anchor(&registry, &ctx, "mixed.txt", &anchor, "BETA")
        .await
        .expect("mixed-ending edit must succeed");

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"before\r\nBETA\r\ngamma\r\ndelta\r\n"
    );
    assert!(
        result.to_string().contains("Mixed line endings"),
        "mixed-ending mutation must expose its preservation warning"
    );
    cleanup(&workdir).await;
}

/// Rewrite invalid UTF-8 as valid replacement text and expose a warning.
#[tokio::test]
async fn edit_rewrites_invalid_utf8_with_warning() {
    let workdir = tempdir();
    let target = workdir.join("invalid.txt");
    tokio::fs::write(&target, b"before\n\xFF\nafter")
        .await
        .unwrap();

    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());
    let anchor = read_anchor(&registry, &ctx, "invalid.txt", "before").await;
    let result = replace_anchor(&registry, &ctx, "invalid.txt", &anchor, "after-before")
        .await
        .expect("invalid UTF-8 rewrite must succeed");

    let bytes = tokio::fs::read(&target).await.unwrap();
    assert!(std::str::from_utf8(&bytes).is_ok());
    assert_eq!(String::from_utf8(bytes).unwrap(), "after-before\n�\nafter");
    assert!(
        result.to_string().contains("Invalid UTF-8"),
        "invalid UTF-8 mutation must expose its rewrite warning"
    );
    cleanup(&workdir).await;
}

/// Reject a missing Edit target without creating bytes or temporary files.
#[tokio::test]
async fn edit_missing_path_fails_without_creating_target_or_temp_files() {
    let workdir = tempdir();
    let target = workdir.join("missing.txt");
    let registry = ToolRegistry::builtins();
    let ctx = ctx_with(workdir.clone());

    let result = append_lines(&registry, &ctx, "missing.txt", &["created"]).await;
    assert!(
        result.is_err(),
        "Edit must direct missing files to the Write tool"
    );
    assert!(!target.exists());
    assert!(temporary_files(&workdir).await.is_empty());
    cleanup(&workdir).await;
}

/// Clean temporary replacement files after a forced post-mutation failure.
#[tokio::test]
async fn edit_forced_formatter_failure_leaves_no_temp_files() {
    let workdir = tempdir();
    let target = workdir.join("formatter.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let formatter = FormatterPlane::new(Arc::new(FailingFormatter));
    let ctx = ctx_with_formatter(workdir.clone(), formatter);
    let registry = ToolRegistry::builtins();
    let anchor = read_anchor(&registry, &ctx, "formatter.txt", "before").await;

    let result = replace_anchor(&registry, &ctx, "formatter.txt", &anchor, "after").await;
    let message = result
        .expect_err("the injected formatter failure must surface as an error")
        .to_string();
    assert!(message.contains("forced formatter failure"));
    // Formatter failures occur after the committed write by contract; the
    // important atomicity invariant here is that no replacement temp survives.
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(temporary_files(&workdir).await.is_empty());
    cleanup(&workdir).await;
}

/// Serialize mutations of one resolved file even when sessions differ.
#[tokio::test(start_paused = true)]
async fn same_resolved_file_serializes_mutations_across_sessions() {
    let workdir = tempdir();
    let target = workdir.join("shared.txt");
    tokio::fs::write(&target, "root\n").await.unwrap();
    let (formatter, mut calls, max_active) = channel_formatter(None);
    let registry = Arc::new(ToolRegistry::builtins());
    let ctx_a = ctx_with_formatter_session(
        workdir.clone(),
        formatter.clone(),
        Some(SessionId::new()),
        CancellationToken::new(),
    );
    let ctx_b = ctx_with_formatter_session(
        workdir.clone(),
        formatter,
        Some(SessionId::new()),
        CancellationToken::new(),
    );

    let first_registry = Arc::clone(&registry);
    let first = tokio::spawn(async move {
        append_lines(first_registry.as_ref(), &ctx_a, "shared.txt", &["first"]).await
    });
    let first_call = calls
        .recv()
        .await
        .expect("first formatter call must arrive");
    assert_eq!(first_call.file, target);

    let second_registry = Arc::clone(&registry);
    let second = tokio::spawn(async move {
        append_lines(second_registry.as_ref(), &ctx_b, "shared.txt", &["second"]).await
    });
    assert_no_formatter_call(&mut calls, "second-session edit").await;

    first_call
        .release
        .send(())
        .expect("first formatter release must arrive");
    first
        .await
        .expect("first session edit task must not panic")
        .expect("first session edit must succeed");

    let second_call = calls
        .recv()
        .await
        .expect("second formatter call must arrive");
    assert_eq!(second_call.file, target);
    second_call
        .release
        .send(())
        .expect("second formatter release must arrive");
    second
        .await
        .expect("second session edit task must not panic")
        .expect("second session edit must succeed");

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"root\nfirst\nsecond\n"
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    cleanup(&workdir).await;
}

/// Serialize mutations through hard-link aliases using one inode identity.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn hard_link_aliases_serialize_mutations_by_inode() {
    use std::os::unix::fs::MetadataExt;

    let workdir = tempdir();
    let target = workdir.join("target.txt");
    let alias = workdir.join("alias.txt");
    tokio::fs::write(&target, "root\n").await.unwrap();
    std::fs::hard_link(&target, &alias).unwrap();
    let inode = std::fs::metadata(&target).unwrap().ino();
    let (formatter, mut calls, max_active) = channel_formatter(None);
    let registry = Arc::new(ToolRegistry::builtins());
    let ctx_a = ctx_with_formatter_session(
        workdir.clone(),
        formatter.clone(),
        Some(SessionId::new()),
        CancellationToken::new(),
    );
    let ctx_b = ctx_with_formatter_session(
        workdir.clone(),
        formatter,
        Some(SessionId::new()),
        CancellationToken::new(),
    );

    let first_registry = Arc::clone(&registry);
    let first = tokio::spawn(async move {
        append_lines(first_registry.as_ref(), &ctx_a, "target.txt", &["first"]).await
    });
    let first_call = calls
        .recv()
        .await
        .expect("first formatter call must arrive");
    assert_eq!(first_call.file, target);

    let second_registry = Arc::clone(&registry);
    let second = tokio::spawn(async move {
        append_lines(second_registry.as_ref(), &ctx_b, "alias.txt", &["second"]).await
    });
    assert_no_formatter_call(&mut calls, "hard-link alias edit").await;

    first_call
        .release
        .send(())
        .expect("first formatter release must arrive");
    first
        .await
        .expect("first hard-link edit task must not panic")
        .expect("first hard-link edit must succeed");

    let second_call = calls
        .recv()
        .await
        .expect("second formatter call must arrive");
    assert_eq!(second_call.file, alias);
    second_call
        .release
        .send(())
        .expect("second formatter release must arrive");
    second
        .await
        .expect("second hard-link edit task must not panic")
        .expect("second hard-link edit must succeed");

    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
    assert_eq!(std::fs::metadata(&alias).unwrap().ino(), inode);
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"root\nfirst\nsecond\n"
    );
    assert_eq!(
        tokio::fs::read(&alias).await.unwrap(),
        b"root\nfirst\nsecond\n"
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    cleanup(&workdir).await;
}

/// Keep Read behind a formatter and expose only the formatter's final bytes.
#[tokio::test(start_paused = true)]
async fn read_waits_for_formatter_and_snapshots_final_bytes() {
    let workdir = tempdir();
    let target = workdir.join("final.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let (formatter, mut calls, max_active) = channel_formatter(Some(FormatterRewrite {
        in_flight: b"formatter-in-flight\n",
        final_bytes: b"formatter-final\n",
    }));
    let registry = Arc::new(ToolRegistry::builtins());
    let edit_ctx = ctx_with_formatter_session(
        workdir.clone(),
        formatter.clone(),
        Some(SessionId::new()),
        CancellationToken::new(),
    );
    let read_ctx = ctx_with_formatter_session(
        workdir.clone(),
        formatter,
        Some(SessionId::new()),
        CancellationToken::new(),
    );

    let edit_registry = Arc::clone(&registry);
    let edit_task = tokio::spawn(async move {
        append_lines(edit_registry.as_ref(), &edit_ctx, "final.txt", &["edit"]).await
    });
    let edit_call = calls.recv().await.expect("edit formatter call must arrive");
    assert_eq!(edit_call.file, target);

    let (read_done, mut read_result_rx) = oneshot::channel();
    let read_registry = Arc::clone(&registry);
    let read_task = tokio::spawn(async move {
        let read = read_registry
            .get("read")
            .expect("builtin Read must be registered");
        let result = read.execute(&read_ctx, json!({"path": "final.txt"})).await;
        let _ = read_done.send(result);
    });
    let before_release = tokio::time::timeout(Duration::from_secs(1), &mut read_result_rx).await;
    let read_was_blocked = before_release.is_err();

    edit_call
        .release
        .send(())
        .expect("edit formatter release must arrive");
    edit_task
        .await
        .expect("edit task must not panic")
        .expect("edit must succeed after formatter release");
    let read_result = match before_release {
        Ok(result) => result.expect("Read task must return a result"),
        Err(_) => read_result_rx
            .await
            .expect("Read completion channel must remain connected"),
    };
    read_task.await.expect("Read task must not panic");

    assert!(
        read_was_blocked,
        "Read must wait for the held Edit formatter"
    );
    let read_result = read_result.expect("Read must succeed after formatter release");
    assert_eq!(read_result["content"].as_str(), Some("formatter-final"));
    let output = read_result["output"]
        .as_str()
        .expect("Read result must contain string output");
    assert!(output.contains("formatter-final"));
    assert!(!output.contains("formatter-in-flight"));
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"formatter-final\n"
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    cleanup(&workdir).await;
}

/// Re-resolve a symlink after waiting and edit only its newly named target.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn waiting_symlink_edit_reresolves_after_retarget() {
    use std::os::unix::fs::symlink;

    let workdir = tempdir();
    let old_target = workdir.join("old.txt");
    let new_target = workdir.join("new.txt");
    let link = workdir.join("current.txt");
    tokio::fs::write(&old_target, "old\n").await.unwrap();
    tokio::fs::write(&new_target, "new\n").await.unwrap();
    symlink("old.txt", &link).unwrap();
    let (formatter, mut calls, max_active) = channel_formatter(None);
    let registry = Arc::new(ToolRegistry::builtins());
    let session = Some(SessionId::new());
    let first_ctx = ctx_with_formatter_session(
        workdir.clone(),
        formatter.clone(),
        session,
        CancellationToken::new(),
    );
    let second_ctx = ctx_with_formatter_session(
        workdir.clone(),
        formatter,
        session,
        CancellationToken::new(),
    );

    let first_registry = Arc::clone(&registry);
    let first = tokio::spawn(async move {
        append_lines(
            first_registry.as_ref(),
            &first_ctx,
            "current.txt",
            &["first"],
        )
        .await
    });
    let first_call = calls
        .recv()
        .await
        .expect("first formatter call must arrive");
    assert_eq!(first_call.file, old_target);

    let second_registry = Arc::clone(&registry);
    let second = tokio::spawn(async move {
        append_lines(
            second_registry.as_ref(),
            &second_ctx,
            "current.txt",
            &["second"],
        )
        .await
    });
    assert_no_formatter_call(&mut calls, "waiting symlink edit").await;

    std::fs::remove_file(&link).unwrap();
    symlink("new.txt", &link).unwrap();
    first_call
        .release
        .send(())
        .expect("first formatter release must arrive");
    first
        .await
        .expect("first symlink edit task must not panic")
        .expect("first symlink edit must succeed");

    let second_call = calls
        .recv()
        .await
        .expect("second formatter call must arrive");
    assert_eq!(second_call.file, new_target);
    second_call
        .release
        .send(())
        .expect("second formatter release must arrive");
    second
        .await
        .expect("second symlink edit task must not panic")
        .expect("second symlink edit must succeed");

    assert_eq!(tokio::fs::read(&old_target).await.unwrap(), b"old\nfirst\n");
    assert_eq!(
        tokio::fs::read(&new_target).await.unwrap(),
        b"new\nsecond\n"
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), PathBuf::from("new.txt"));
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    cleanup(&workdir).await;
}

/// Cancel an Edit while it waits and prove that it never reaches the formatter.
#[tokio::test(start_paused = true)]
async fn cancelled_waiting_edit_returns_cancelled_without_commit() {
    let workdir = tempdir();
    let target = workdir.join("cancel.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let (formatter, mut calls, max_active) = channel_formatter(None);
    let registry = Arc::new(ToolRegistry::builtins());
    let session = SessionId::new();
    let first_ctx = ctx_with_formatter_session(
        workdir.clone(),
        formatter.clone(),
        Some(session),
        CancellationToken::new(),
    );
    let cancel = CancellationToken::new();
    let mut second_ctx =
        ctx_with_formatter_session(workdir.clone(), formatter, Some(session), cancel.clone());
    let (permission, mut permission_requests) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Read, "*"),
        Rule::new(Action::Edit, "*", Mode::Ask),
    ]));
    second_ctx.permission = permission.for_session(session);

    let first_registry = Arc::clone(&registry);
    let first = tokio::spawn(async move {
        append_lines(
            first_registry.as_ref(),
            &first_ctx,
            "cancel.txt",
            &["first"],
        )
        .await
    });
    let first_call = calls
        .recv()
        .await
        .expect("first formatter call must arrive");
    assert_eq!(first_call.file, target);

    let second_registry = Arc::clone(&registry);
    let mut second = tokio::spawn(async move {
        append_lines(
            second_registry.as_ref(),
            &second_ctx,
            "cancel.txt",
            &["cancelled"],
        )
        .await
    });
    let request = permission_requests
        .recv()
        .await
        .expect("second Edit permission request must arrive");
    assert_eq!(request.action, Action::Edit);
    assert_eq!(request.session, Some(session));
    request
        .reply
        .send(Decision::AllowOnce)
        .expect("second Edit permission reply must arrive");
    assert_no_formatter_call(&mut calls, "cancelled waiting edit").await;

    cancel.cancel();
    let cancellation = tokio::time::timeout(Duration::from_secs(1), &mut second).await;
    let cancellation_was_observed_while_waiting = cancellation.is_ok();
    first_call
        .release
        .send(())
        .expect("first formatter release must arrive");
    first
        .await
        .expect("first edit task must not panic")
        .expect("first edit must succeed");
    let second_result = match cancellation {
        Ok(joined) => joined.expect("cancelled Edit task must not panic"),
        Err(_) => second
            .await
            .expect("cancelled Edit task must not panic after lock release"),
    };

    assert!(
        cancellation_was_observed_while_waiting,
        "a waiting Edit must observe cancellation before the held formatter releases"
    );
    assert!(matches!(second_result, Err(ToolError::Cancelled)));
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"before\nfirst\n");
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    cleanup(&workdir).await;
}
