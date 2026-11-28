//! Workspace filesystem and Git worktree APIs for frontend clients.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{
    ApiMethod, ApiScope, HostError, NativeApiEndpoint, NativeBundleApiHandler,
    NativeBundleApiReply, NativeBundleApiRequest,
};
use serde_json::{Value, json};

mod files;
mod worktrees;

struct WorkspaceApi {
    endpoints: Vec<NativeApiEndpoint>,
}

impl WorkspaceApi {
    fn new() -> Self {
        use ApiMethod::{Delete, Get, Post};
        let endpoints = [
            ("read-file", Get, "/fs/read", "Read file bytes."),
            ("list-directory", Get, "/fs/list", "List directory entries."),
            ("find-files", Get, "/fs/find", "Find paths by glob."),
            (
                "search-text",
                Get,
                "/fs/search",
                "Search files for literal text.",
            ),
            ("list-worktrees", Get, "/worktrees", "List Git worktrees."),
            (
                "create-worktree",
                Post,
                "/worktrees",
                "Create a Git worktree.",
            ),
            (
                "delete-worktree",
                Delete,
                "/worktrees/{worktree}",
                "Remove a Git worktree.",
            ),
            (
                "reset-worktree",
                Post,
                "/worktrees/{worktree}/reset",
                "Clean and reset a Git worktree.",
            ),
        ]
        .into_iter()
        .map(|(id, method, path, description)| NativeApiEndpoint {
            id: id.to_owned(),
            method,
            scope: ApiScope::Global,
            path: path.to_owned(),
            description: description.to_owned(),
        })
        .collect();
        Self { endpoints }
    }

    async fn call(&self, request: NativeBundleApiRequest) -> Result<Value, HostError> {
        match request.api.as_str() {
            "read-file" => files::read(&request.query).await,
            "list-directory" => files::list(&request.query).await,
            "find-files" => files::find(&request.query),
            "search-text" => files::search_text(&request.query),
            "list-worktrees" | "create-worktree" | "delete-worktree" | "reset-worktree" => {
                worktrees::call(&request).await
            }
            _ => unreachable!("the BundleApi router selects a declared endpoint"),
        }
    }
}

#[async_trait]
impl NativeBundleApiHandler for WorkspaceApi {
    fn id(&self) -> &str {
        "workspace-files"
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let (status, body) = match self.call(request).await {
            Ok(body) => (200, body),
            Err(error) => {
                let (status, code, message) = match error {
                    HostError::InvalidInput(message) => (400, "invalid_argument", message),
                    HostError::NotFound(message) => (404, "not_found", message),
                    HostError::Internal(message) => (500, "internal", message),
                    other => return Err(other),
                };
                (
                    status,
                    json!({ "error": { "code": code, "message": message } }),
                )
            }
        };
        Ok(NativeBundleApiReply { status, body })
    }
}

fn directory(requested: Option<&str>) -> Result<&Path, HostError> {
    let named = requested.unwrap_or_default().trim();
    if named.is_empty() {
        return Err(HostError::InvalidInput(
            "this endpoint needs a directory scope: set the request's `directory` field (hya serve has no working directory)".into(),
        ));
    }
    let path = Path::new(named);
    if !path.is_absolute() {
        return Err(HostError::InvalidInput(format!(
            "the directory scope must be an absolute path, got `{named}`"
        )));
    }
    Ok(path)
}

hya_runtime_abi::export_native_bundle_v2!(|_context, out| {
    out.api_handlers.push(Arc::new(WorkspaceApi::new()));
});
