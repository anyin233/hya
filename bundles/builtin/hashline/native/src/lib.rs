//! Native ABI v2 bundle (`hya/hashline`) providing content-hashed line-addressed
//! `read`, `write`, `edit`, and `grep` tools overriding `hya-bare`'s standard
//! file primitives.
//!
//! Custom URI schemes (such as `channel://`) are delegated generically through
//! [`hya_tool::handle::HandleRouter`]; post-edit formatting and LSP diagnostics
//! run via host/bundle hooks rather than hardcoded calls inside this library.

use std::path::Path;
use std::sync::Arc;

use hya_tool::handle::HandleError;
use hya_tool::{ProjectScope, Tool, ToolCtx, ToolError};

mod edit;
mod file_diff;
mod grep;
mod hashline;
mod read;
mod read_media;
mod write;

fn closed_obj_schema(
    name: &str,
    description: &str,
    properties: serde_json::Value,
    required: &[&str],
) -> hya_proto::ToolSchema {
    let mut schema = hya_tool::tool::obj_schema(name, description, properties, required);
    let serde_json::Value::Object(input) = &mut schema.input_schema else {
        unreachable!("obj_schema always produces an object")
    };
    // Preserve the field order of existing schemas sent to providers.
    input.shift_insert(
        1,
        "additionalProperties".into(),
        serde_json::Value::Bool(false),
    );
    schema
}

/// Require `ExternalDirectory` permission when `target` lies outside every
/// Project root (ADR-0026).
pub(crate) async fn assert_external_directory(
    ctx: &ToolCtx,
    target: &Path,
    is_directory: bool,
) -> Result<(), ToolError> {
    ProjectScope::for_ctx(ctx)
        .authorize(&ctx.permission, target, |scope| {
            if is_directory {
                scope.outside_directory_pattern(target)
            } else {
                scope.outside_dir_pattern(target)
            }
        })
        .await?;
    Ok(())
}

/// Authorize an external Grep target using the kind-blind parent pattern so
/// callers cannot probe path existence outside `ProjectScope` before approval.
pub(crate) async fn assert_external_search_target(
    ctx: &ToolCtx,
    target: &Path,
) -> Result<(), ToolError> {
    assert_external_directory(ctx, target, false).await
}

/// Map a [`HandleError`] into a stable [`ToolError`].
pub(crate) fn map_handle_error(error: HandleError) -> ToolError {
    match error {
        HandleError::NotFound(handle) => ToolError::Other(format!("{handle} does not exist")),
        HandleError::PermissionDenied(msg) => ToolError::Other(format!("permission denied: {msg}")),
        HandleError::Io(io_err) => ToolError::Io(io_err),
        HandleError::Handler(msg) => ToolError::Other(msg),
        other => ToolError::Input(other.to_string()),
    }
}

/// Construct the four hashline tools sharing one [`hashline::HashlineRuntime`].
#[must_use]
pub fn hashline_tools() -> Vec<Arc<dyn Tool>> {
    let runtime = Arc::new(hashline::HashlineRuntime::new());
    vec![
        Arc::new(read::ReadTool::new(Arc::clone(&runtime))),
        Arc::new(write::WriteTool::new(Arc::clone(&runtime))),
        Arc::new(edit::EditTool::new(Arc::clone(&runtime))),
        Arc::new(grep::GrepTool::with_runtime(Arc::clone(&runtime))),
    ]
}

hya_runtime_abi::export_native_bundle_v2!(|_ctx, out| {
    out.tools.extend(hashline_tools());
});

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use hya_runtime_abi::{
        HostError, UriContent, UriResolveCtx, UriSchemaHandler, adapt_uri_schema_handler,
    };
    use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("hya-hashline-{label}-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct TestUriHandler {
        scheme: &'static str,
        writable: bool,
        stored: Arc<Mutex<String>>,
    }

    #[async_trait]
    impl UriSchemaHandler for TestUriHandler {
        fn scheme(&self) -> &str {
            self.scheme
        }

        fn writable(&self) -> bool {
            self.writable
        }

        async fn read(&self, _ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
            let body = self.stored.lock().unwrap().clone();
            Ok(UriContent {
                body: format!("{uri} => {body}"),
                media_type: "text/plain".to_string(),
                metadata: Some(json!({ "source": "uri-handler" })),
            })
        }

        async fn write(
            &self,
            _ctx: UriResolveCtx<'_>,
            uri: &str,
            content: &str,
        ) -> Result<Value, HostError> {
            *self.stored.lock().unwrap() = content.to_string();
            Ok(json!({ "output": format!("stored {} bytes at {uri}", content.len()) }))
        }
    }

    fn make_ctx(workdir: &Path) -> ToolCtx {
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
            Rule::new(Action::Read, "*", Mode::Allow),
            Rule::new(Action::Edit, "*", Mode::Allow),
            Rule::new(Action::Grep, "*", Mode::Allow),
        ]));
        ToolCtx::new(workdir.to_path_buf(), permission, CancellationToken::new())
    }

    fn tool_by_name(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .cloned()
            .unwrap_or_else(|| panic!("missing tool {name}"))
    }

    #[tokio::test]
    async fn hashline_read_write_edit_grep_roundtrip() {
        let dir = TempDir::new("roundtrip");
        let ctx = make_ctx(&dir.0);

        let tools = hashline_tools();
        let read = tool_by_name(&tools, "read");
        let write = tool_by_name(&tools, "write");
        let edit = tool_by_name(&tools, "edit");
        let grep = tool_by_name(&tools, "grep");

        // Write a file.
        let written = write
            .execute(
                &ctx,
                json!({ "path": "demo.txt", "content": "alpha\nbeta\ngamma\n" }),
            )
            .await
            .unwrap();
        assert_eq!(written["ok"], true);

        // Read produces LINE#HASH anchors.
        let read_out = read
            .execute(&ctx, json!({ "path": "demo.txt" }))
            .await
            .unwrap();
        let output = read_out["output"].as_str().unwrap();
        let line2 = output
            .lines()
            .find(|line| line.starts_with("2#") && line.ends_with(":beta"))
            .expect("line 2 hashline anchor");
        let anchor2 = line2.split(':').next().unwrap().to_string();

        // Edit using the anchor from read.
        let edited = edit
            .execute(
                &ctx,
                json!({
                    "path": "demo.txt",
                    "edits": [{ "op": "replace", "pos": anchor2, "lines": ["BETA_UPDATED"] }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(edited["metadata"]["classification"], "applied");
        assert_eq!(
            std::fs::read_to_string(dir.0.join("demo.txt")).unwrap(),
            "alpha\nBETA_UPDATED\ngamma\n"
        );

        // Grep returns hashline-anchored matches.
        let grepped = grep
            .execute(
                &ctx,
                json!({ "pattern": "BETA_UPDATED", "path": "demo.txt" }),
            )
            .await
            .unwrap();
        let grep_out = grepped["output"].as_str().unwrap();
        assert!(
            grep_out.contains("2#") && grep_out.contains("BETA_UPDATED"),
            "{grep_out}"
        );
    }

    #[tokio::test]
    async fn custom_uri_schemes_delegate_through_handle_router_with_no_hardcoded_channel() {
        let dir = TempDir::new("uri-router");
        let ctx = make_ctx(&dir.0);
        let tools = hashline_tools();
        let read = tool_by_name(&tools, "read");
        let write = tool_by_name(&tools, "write");

        // Unregistered `channel://` is NOT hardcoded — it fails as an unknown scheme.
        let err = read
            .execute(&ctx, json!({ "path": "channel://DM-12345678" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown") || err.to_string().contains("channel"),
            "unexpected error: {err}"
        );

        // Register a dynamic `UriSchemaHandler` on HandleRouter and verify read + write delegation.
        let stored = Arc::new(Mutex::new("initial".to_string()));
        let rw_handler = adapt_uri_schema_handler(
            Arc::new(TestUriHandler {
                scheme: "memo",
                writable: true,
                stored: Arc::clone(&stored),
            }),
            None,
        );
        let ro_handler = adapt_uri_schema_handler(
            Arc::new(TestUriHandler {
                scheme: "channel",
                writable: false,
                stored: Arc::new(Mutex::new("mail-body".to_string())),
            }),
            None,
        );
        let ctx = ctx
            .with_uri_handler(rw_handler)
            .unwrap()
            .with_uri_handler(ro_handler)
            .unwrap();

        let channel_read = read
            .execute(&ctx, json!({ "path": "channel://main/scout" }))
            .await
            .unwrap();
        assert_eq!(channel_read["output"], "channel://main/scout => mail-body");
        assert_eq!(channel_read["metadata"]["path"], "channel://main/scout");

        // Read-only custom scheme rejects write.
        assert!(
            write
                .execute(
                    &ctx,
                    json!({ "path": "channel://main/scout", "content": "nope" })
                )
                .await
                .is_err()
        );

        // Writable custom scheme writes and reads back cleanly.
        let memo_write = write
            .execute(
                &ctx,
                json!({ "path": "memo://note-1", "content": "hello world" }),
            )
            .await
            .unwrap();
        assert_eq!(memo_write["ok"], true);
        assert_eq!(memo_write["output"], "stored 11 bytes at memo://note-1");

        let memo_read = read
            .execute(&ctx, json!({ "path": "memo://note-1" }))
            .await
            .unwrap();
        assert_eq!(memo_read["output"], "memo://note-1 => hello world");
    }
}
