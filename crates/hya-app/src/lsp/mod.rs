//! App-owned language-server lifecycle behind the existing tool LSP plane.
mod config;
mod transport;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_tool::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, watch};

use config::ServerConfig;
use transport::{Client, file_uri};

type ClientSlot = Arc<OnceCell<Result<Arc<Client>, String>>>;

pub(crate) fn load_plane() -> anyhow::Result<LspPlane> {
    let servers = config::load()?;
    if servers.is_empty() {
        return Ok(LspPlane::default());
    }
    Ok(LspPlane::new(Arc::new(ProcessLspProvider {
        servers,
        clients: Mutex::new(BTreeMap::new()),
        updates: watch::channel(0).0,
    })))
}

struct ProcessLspProvider {
    servers: BTreeMap<String, ServerConfig>,
    clients: Mutex<BTreeMap<(String, PathBuf), ClientSlot>>,
    updates: watch::Sender<u64>,
}

impl ProcessLspProvider {
    async fn clients_for(&self, file: &Path) -> Result<Vec<Arc<Client>>, LspError> {
        let directory = file.is_dir();
        let mut clients = Vec::new();
        let mut errors = Vec::new();
        for (id, config) in &self.servers {
            let roots: Vec<PathBuf> = if directory {
                let active: Vec<_> = self
                    .clients
                    .lock()
                    .await
                    .keys()
                    .filter(|(known, root)| {
                        known == id && (file.starts_with(root) || root.starts_with(file))
                    })
                    .map(|(_, root)| root.clone())
                    .collect();
                if !active.is_empty() {
                    active
                } else if config.applies_to_directory(file) {
                    vec![config.root(file)]
                } else {
                    continue;
                }
            } else if config.matches(file) {
                vec![config.root(file)]
            } else {
                continue;
            };
            for root in roots {
                let slot = self
                    .clients
                    .lock()
                    .await
                    .entry((id.clone(), root.clone()))
                    .or_default()
                    .clone();
                let initializing = slot.get().is_none();
                let result = slot
                    .get_or_init(|| async {
                        Client::start(config, &root, self.updates.clone())
                            .await
                            .map_err(|error| error.to_string())
                    })
                    .await;
                // The initialized slot is visible before subscribers query its status.
                if initializing {
                    self.updates
                        .send_modify(|generation| *generation = generation.wrapping_add(1));
                }
                match result {
                    Ok(client) if client.failure().is_none() => clients.push(client.clone()),
                    Ok(client) => errors.push(
                        client
                            .failure()
                            .unwrap_or_else(|| "LSP connection failed".into()),
                    ),
                    Err(error) => errors.push(error.clone()),
                }
            }
        }
        if clients.is_empty() && !errors.is_empty() {
            return Err(LspError(errors.join("; ")));
        }
        for error in errors {
            tracing::warn!(%error, "one LSP client is unavailable; using other matching clients");
        }
        Ok(clients)
    }

    async fn sync_document(
        client: &Client,
        file: &Path,
        kind: &str,
    ) -> Result<Option<(String, i32)>, LspError> {
        let uri = file_uri(file)?;
        let mut documents = client.documents.lock().await;
        let text = match tokio::fs::read_to_string(file).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if documents.remove(&uri).is_some() {
                    client
                        .notify(
                            "textDocument/didClose",
                            json!({"textDocument": {"uri": uri}}),
                        )
                        .await?;
                    client.document_closed(&uri);
                }
                return Ok(None);
            }
            Err(error) => {
                return Err(LspError(format!(
                    "read LSP document {}: {error}",
                    file.display()
                )));
            }
        };
        let version = match documents.get(&uri) {
            Some((version, previous)) if previous == &text => return Ok(Some((uri, *version))),
            Some((version, _)) => {
                let version = version
                    .checked_add(1)
                    .ok_or_else(|| LspError("LSP document version exhausted".into()))?;
                client.document_changed(&uri, version);
                client.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]})).await?;
                version
            }
            None => {
                client.document_changed(&uri, 1);
                client.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": language_id(file), "version": 1, "text": text}})).await?;
                1
            }
        };
        if kind != "read" {
            client
                .notify(
                    "textDocument/didSave",
                    json!({"textDocument": {"uri": uri}, "text": text}),
                )
                .await?;
        }
        documents.insert(uri.clone(), (version, text));
        Ok(Some((uri, version)))
    }

    async fn execute_client(
        client: &Client,
        request: &LspRequest,
        method: &str,
    ) -> Result<Vec<Value>, LspError> {
        if !request.file.is_dir() {
            Self::sync_document(client, &request.file, "read").await?;
        }
        let uri = file_uri(&request.file)?;
        let mut params = json!({"textDocument": {"uri": uri}, "position": {"line": request.line, "character": request.character}});
        match request.operation {
            LspOperation::FindReferences => params["context"] = json!({"includeDeclaration": true}),
            LspOperation::DocumentSymbol => params = json!({"textDocument": {"uri": uri}}),
            LspOperation::WorkspaceSymbol => {
                params = json!({"query": request.query.as_deref().unwrap_or("")})
            }
            _ => {}
        }
        let prepared = client.request(method, params).await?;
        if matches!(
            request.operation,
            LspOperation::IncomingCalls | LspOperation::OutgoingCalls
        ) {
            let method = if request.operation == LspOperation::IncomingCalls {
                "callHierarchy/incomingCalls"
            } else {
                "callHierarchy/outgoingCalls"
            };
            let mut results = Vec::new();
            for item in result_items(prepared) {
                results.extend(result_items(
                    client.request(method, json!({"item": item})).await?,
                ));
            }
            Ok(results)
        } else {
            Ok(result_items(prepared))
        }
    }
}

#[async_trait]
impl LspProvider for ProcessLspProvider {
    async fn has_clients(&self, file: &Path) -> Result<bool, LspError> {
        Ok(!self.clients_for(file).await?.is_empty())
    }

    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
        let clients = self.clients_for(&request.file).await?;
        if clients.is_empty() {
            if request.operation == LspOperation::WorkspaceSymbol {
                return Ok(Vec::new());
            }
            return Err(LspError(
                "No LSP server available for this file type.".into(),
            ));
        }
        let method = match request.operation {
            LspOperation::GoToDefinition => "textDocument/definition",
            LspOperation::FindReferences => "textDocument/references",
            LspOperation::Hover => "textDocument/hover",
            LspOperation::DocumentSymbol => "textDocument/documentSymbol",
            LspOperation::WorkspaceSymbol => "workspace/symbol",
            LspOperation::GoToImplementation => "textDocument/implementation",
            LspOperation::PrepareCallHierarchy
            | LspOperation::IncomingCalls
            | LspOperation::OutgoingCalls => "textDocument/prepareCallHierarchy",
        };
        let mut results = Vec::new();
        let mut completed = false;
        let mut errors = Vec::new();
        for client in clients {
            if !client.supports(method) {
                continue;
            }
            match Self::execute_client(&client, &request, method).await {
                Ok(values) => {
                    completed = true;
                    results.extend(values);
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        if !completed {
            return Err(LspError(if errors.is_empty() {
                format!(
                    "Configured LSP servers do not support {}",
                    request.operation.as_str()
                )
            } else {
                errors.join("; ")
            }));
        }
        for error in errors {
            tracing::warn!(%error, "one LSP operation failed; preserving other client results");
        }
        Ok(results)
    }

    async fn touch_file(&self, file: &Path, kind: &str) -> Result<(), LspError> {
        for client in self.clients_for(file).await? {
            if let Some((uri, version)) = Self::sync_document(&client, file, kind).await? {
                client.wait_diagnostics(&uri, version).await?;
            }
        }
        Ok(())
    }

    async fn diagnostics(&self, workdir: &Path, targets: &[&Path]) -> Result<Value, LspError> {
        let clients = self.clients.lock().await;
        let mut merged = serde_json::Map::new();
        for ((_, root), slot) in clients.iter() {
            if !workdir.starts_with(root)
                && !root.starts_with(workdir)
                && !targets.iter().any(|target| target.starts_with(root))
            {
                continue;
            }
            let Some(Ok(client)) = slot.get() else {
                continue;
            };
            for (path, mut rows) in client.diagnostics(workdir, targets) {
                match merged.entry(path) {
                    serde_json::map::Entry::Vacant(entry) => {
                        entry.insert(rows);
                    }
                    serde_json::map::Entry::Occupied(mut entry) => {
                        if let (Some(target), Some(rows)) =
                            (entry.get_mut().as_array_mut(), rows.as_array_mut())
                        {
                            target.append(rows);
                        }
                    }
                }
            }
        }
        Ok(Value::Object(merged))
    }

    fn subscribe(&self) -> Option<watch::Receiver<u64>> {
        Some(self.updates.subscribe())
    }

    async fn status(&self, workdir: &Path) -> Result<Vec<Value>, LspError> {
        let clients = self.clients.lock().await;
        let mut rows = Vec::new();
        for ((id, root), slot) in clients.iter() {
            if !workdir.starts_with(root) && !root.starts_with(workdir) {
                continue;
            }
            let Some(result) = slot.get() else {
                continue;
            };
            let error = match result {
                Ok(client) => client.failure(),
                Err(error) => Some(error.clone()),
            };
            let mut row = json!({"id": id, "name": id, "root": root, "status": if error.is_some() { "error" } else { "connected" }});
            if let Some(error) = error {
                row["error"] = Value::String(error);
            }
            rows.push(row);
        }
        Ok(rows)
    }
}

fn result_items(value: Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(values) => values,
        value => vec![value],
    }
}

fn language_id(file: &Path) -> &str {
    match file
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
    {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "rs" => "rust",
        "py" | "pyi" => "python",
        "sh" | "bash" => "shellscript",
        "cc" | "cpp" | "hpp" => "cpp",
        extension => extension,
    }
}
