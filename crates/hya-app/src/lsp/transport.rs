//! Content-Length JSON-RPC transport with correlated requests and owned teardown.
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use hya_tool::LspError;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::config::ServerConfig;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
type Reply = oneshot::Sender<Result<Value, String>>;

#[derive(Default)]
struct DiagnosticState {
    expected_version: Option<i32>,
    publication: Option<(Option<i32>, Value)>,
}

struct Shared {
    writer: Mutex<ChildStdin>,
    pending: StdMutex<HashMap<u64, Reply>>,
    diagnostics: StdMutex<HashMap<String, DiagnosticState>>,
    diagnostic_updates: watch::Sender<u64>,
    updates: watch::Sender<u64>,
    failure: StdMutex<Option<String>>,
    shutdown: CancellationToken,
}

pub(super) struct Client {
    shared: Arc<Shared>,
    next_id: AtomicU64,
    capabilities: Value,
    pub documents: Mutex<HashMap<String, (i32, String)>>,
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn signal(sender: &watch::Sender<u64>) {
    sender.send_modify(|generation| *generation = generation.wrapping_add(1));
}

pub(super) fn file_uri(path: &Path) -> Result<String, LspError> {
    reqwest::Url::from_file_path(path)
        .map(String::from)
        .map_err(|()| LspError(format!("cannot encode LSP file URI: {}", path.display())))
}

impl Shared {
    fn fail(&self, error: String) {
        let mut failure = lock(&self.failure);
        if failure.is_none() {
            *failure = Some(error.clone());
            signal(&self.updates);
        }
        drop(failure);
        for (_, reply) in lock(&self.pending).drain() {
            let _ = reply.send(Err(error.clone()));
        }
        self.shutdown.cancel();
    }

    async fn send(&self, message: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        if body.len() > MAX_FRAME_BYTES {
            return Err("outgoing LSP frame exceeds 16 MiB".into());
        }
        let mut writer = self.writer.lock().await;
        if let Some(error) = lock(&self.failure).clone() {
            return Err(error);
        }
        // Cancellation after the first byte poisons the stream, including task cancellation.
        let mut guard = FrameWrite {
            shared: self,
            complete: false,
        };
        writer
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&body)
            .await
            .map_err(|error| error.to_string())?;
        writer.flush().await.map_err(|error| error.to_string())?;
        guard.complete = true;
        Ok(())
    }
}

struct FrameWrite<'a> {
    shared: &'a Shared,
    complete: bool,
}
impl Drop for FrameWrite<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.shared
                .fail("LSP framed write failed or was interrupted".into());
        }
    }
}

impl Client {
    pub(super) async fn start(
        config: &ServerConfig,
        root: &Path,
        updates: watch::Sender<u64>,
    ) -> Result<Arc<Self>, LspError> {
        let (program, arguments) = config
            .command
            .split_first()
            .ok_or_else(|| LspError("empty LSP command".into()))?;
        let uri = file_uri(root)?;
        let folders = json!([{"uri": uri, "name": root.file_name().and_then(|s| s.to_str()).unwrap_or("workspace")}]);
        let mut command = Command::new(program);
        command
            .args(arguments)
            .envs(&config.environment)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| LspError(format!("start LSP {program}: {error}")))?;
        let writer = child
            .stdin
            .take()
            .ok_or_else(|| LspError("missing LSP stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError("missing LSP stdout".into()))?;
        let shared = Arc::new(Shared {
            writer: Mutex::new(writer),
            pending: StdMutex::new(HashMap::new()),
            diagnostics: StdMutex::new(HashMap::new()),
            diagnostic_updates: watch::channel(0).0,
            updates,
            failure: StdMutex::new(None),
            shutdown: CancellationToken::new(),
        });
        let reader = tokio::spawn(read_loop(
            stdout,
            shared.clone(),
            config.settings.clone(),
            folders.clone(),
        ));
        tokio::spawn(supervise(child, reader, shared.clone()));
        let mut client = Self {
            shared,
            next_id: AtomicU64::new(1),
            capabilities: Value::Null,
            documents: Mutex::new(HashMap::new()),
        };
        let initialized = client.request("initialize", json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "hya", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": uri, "workspaceFolders": folders,
            "initializationOptions": config.initialization_options,
            "capabilities": {
                "workspace": {"configuration": true, "workspaceFolders": true},
                "textDocument": {"hover": {"contentFormat": ["markdown", "plaintext"]}, "callHierarchy": {}, "publishDiagnostics": {"versionSupport": true}, "diagnostic": {}},
                "general": {"positionEncodings": ["utf-16"]}
            }
        })).await?;
        client.capabilities = initialized
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null);
        client.notify("initialized", json!({})).await?;
        if !config.settings.is_null() {
            client
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings": config.settings}),
                )
                .await?;
        }
        Ok(Arc::new(client))
    }

    pub(super) fn failure(&self) -> Option<String> {
        lock(&self.shared.failure).clone()
    }

    pub(super) fn supports(&self, method: &str) -> bool {
        let capability = match method {
            "textDocument/definition" => "definitionProvider",
            "textDocument/references" => "referencesProvider",
            "textDocument/hover" => "hoverProvider",
            "textDocument/documentSymbol" => "documentSymbolProvider",
            "workspace/symbol" => "workspaceSymbolProvider",
            "textDocument/implementation" => "implementationProvider",
            "textDocument/prepareCallHierarchy" => "callHierarchyProvider",
            "textDocument/diagnostic" => "diagnosticProvider",
            _ => return true,
        };
        self.capabilities[capability] == true || self.capabilities[capability].is_object()
    }

    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        if let Some(error) = self.failure() {
            return Err(LspError(error));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        lock(&self.shared.pending).insert(id, reply);
        let _guard = PendingRequest {
            shared: &self.shared,
            id,
        };
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
            message["params"] = params;
            self.shared.send(&message).await?;
            drop(message);
            response
                .await
                .map_err(|_| "LSP reply channel closed".to_string())?
        })
        .await;
        match result {
            Ok(result) => result.map_err(LspError),
            Err(_) => {
                let error = format!(
                    "LSP {method} timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                );
                self.shared.fail(error.clone());
                Err(LspError(error))
            }
        }
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        message["params"] = params;
        match tokio::time::timeout(REQUEST_TIMEOUT, self.shared.send(&message)).await {
            Ok(result) => result.map_err(LspError),
            Err(_) => {
                let error = format!("LSP {method} write timed out");
                self.shared.fail(error.clone());
                Err(LspError(error))
            }
        }
    }

    pub(super) fn document_changed(&self, uri: &str, version: i32) {
        lock(&self.shared.diagnostics).insert(
            uri.to_owned(),
            DiagnosticState {
                expected_version: Some(version),
                publication: None,
            },
        );
    }

    pub(super) fn document_closed(&self, uri: &str) {
        lock(&self.shared.diagnostics).remove(uri);
    }

    pub(super) async fn wait_diagnostics(&self, uri: &str, version: i32) -> Result<(), LspError> {
        if self.supports("textDocument/diagnostic") {
            let mut params = json!({"textDocument": {"uri": uri}});
            if let Some(identifier) = self.capabilities["diagnosticProvider"].get("identifier") {
                params["identifier"] = identifier.clone();
            }
            let report = self.request("textDocument/diagnostic", params).await?;
            let rows = report
                .get("items")
                .filter(|rows| rows.is_array())
                .ok_or_else(|| LspError("LSP diagnostic response has no full items".into()))?;
            let mut diagnostics = lock(&self.shared.diagnostics);
            let state = diagnostics
                .get_mut(uri)
                .ok_or_else(|| LspError("LSP document closed during diagnostics".into()))?;
            if state.expected_version != Some(version) {
                return Err(LspError("LSP document changed during diagnostics".into()));
            }
            state.publication = Some((Some(version), rows.clone()));
            return Ok(());
        }
        let mut updates = self.shared.diagnostic_updates.subscribe();
        let deadline = tokio::time::Instant::now() + DIAGNOSTIC_TIMEOUT;
        loop {
            if let Some(error) = self.failure() {
                return Err(LspError(error));
            }
            {
                let diagnostics = lock(&self.shared.diagnostics);
                let state = diagnostics
                    .get(uri)
                    .ok_or_else(|| LspError("LSP document closed during diagnostics".into()))?;
                if state.expected_version != Some(version) {
                    return Err(LspError("LSP document changed during diagnostics".into()));
                }
                if matches!(&state.publication, Some((Some(published), _)) if *published == version)
                {
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    // Unversioned push servers have no completion signal. Collect for the full
                    // bounded window, including empty-clear followed by delayed analysis.
                    return if state.publication.is_some() {
                        Ok(())
                    } else {
                        Err(LspError("LSP diagnostics were not published within 2s; current analysis is unavailable".into()))
                    };
                }
            }
            tokio::select! {
                _ = self.shared.shutdown.cancelled() => {},
                _ = tokio::time::sleep_until(deadline) => {},
                result = updates.changed() => { if result.is_err() { return Err(LspError("LSP diagnostics channel closed".into())); } },
            }
        }
    }

    pub(super) fn diagnostics(
        &self,
        workdir: &Path,
        targets: &[&Path],
    ) -> serde_json::Map<String, Value> {
        lock(&self.shared.diagnostics)
            .iter()
            .filter_map(|(uri, state)| {
                let path = reqwest::Url::parse(uri).ok()?.to_file_path().ok()?;
                if !path.starts_with(workdir) && !targets.contains(&path.as_path()) {
                    return None;
                }
                let (_, rows) = state.publication.as_ref()?;
                Some((path.to_string_lossy().into_owned(), rows.clone()))
            })
            .collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
    }
}

struct PendingRequest<'a> {
    shared: &'a Shared,
    id: u64,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        lock(&self.shared.pending).remove(&self.id);
    }
}

struct ProcessGroup(Option<u32>);
impl ProcessGroup {
    fn terminate(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: this adapter spawned this child as its own process-group leader;
            // the guard is cleared immediately after the child is reaped.
            let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(pid, error = %std::io::Error::last_os_error(), "terminate LSP process group");
            }
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

async fn supervise(mut child: Child, reader: tokio::task::JoinHandle<()>, shared: Arc<Shared>) {
    let mut group = ProcessGroup(child.id());
    let reason = tokio::select! {
        result = child.wait() => format!("language server exited: {result:?}"),
        _ = shared.shutdown.cancelled() => "language server stopped".to_owned(),
    };
    shared.fail(reason);
    group.terminate();
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    group.0 = None;
    reader.abort();
    let _ = reader.await;
}

async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    let mut length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let count = (&mut *reader)
            .take((8193 - header_bytes) as u64)
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("language server closed stdout".into());
        }
        header_bytes += count;
        if header_bytes > 8192 {
            return Err("LSP header exceeds 8 KiB".into());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("malformed LSP header")?;
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("duplicate LSP Content-Length".into());
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid LSP Content-Length")?,
            );
        }
    }
    let length = length
        .filter(|length| *length <= MAX_FRAME_BYTES)
        .ok_or("missing or oversized LSP Content-Length")?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body).map_err(|error| format!("invalid LSP JSON: {error}"))
}

async fn read_loop(stdout: ChildStdout, shared: Arc<Shared>, settings: Value, folders: Value) {
    let mut reader = BufReader::new(stdout);
    let error = loop {
        let message = match read_frame(&mut reader).await {
            Ok(value) => value,
            Err(error) => break error,
        };
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id") {
                let response = server_request(method, &message["params"], id, &settings, &folders);
                match tokio::time::timeout(REQUEST_TIMEOUT, shared.send(&response)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break error,
                    Err(_) => break "LSP server-request reply timed out".into(),
                }
            } else if method == "textDocument/publishDiagnostics" {
                let params = &message["params"];
                if let (Some(uri), Some(rows)) =
                    (params["uri"].as_str(), params["diagnostics"].as_array())
                {
                    let version = params["version"]
                        .as_i64()
                        .and_then(|v| i32::try_from(v).ok());
                    let mut diagnostics = lock(&shared.diagnostics);
                    let state = diagnostics.entry(uri.to_owned()).or_default();
                    if version
                        .zip(state.expected_version)
                        .is_some_and(|(published, expected)| published != expected)
                    {
                        continue;
                    }
                    state.publication = Some((version, Value::Array(rows.clone())));
                    drop(diagnostics);
                    signal(&shared.diagnostic_updates);
                }
            }
        } else if let Some(id) = message.get("id").and_then(Value::as_u64)
            && let Some(reply) = lock(&shared.pending).remove(&id)
        {
            let result = if let Some(error) = message.get("error") {
                Err(format!("language server error: {error}"))
            } else {
                message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "LSP response has no result".to_string())
            };
            let _ = reply.send(result);
        }
    };
    shared.fail(error);
}

fn server_request(
    method: &str,
    params: &Value,
    id: &Value,
    settings: &Value,
    folders: &Value,
) -> Value {
    let result = match method {
        "workspace/configuration" => {
            Value::Array(params["items"].as_array().map_or_else(Vec::new, |items| {
                items
                    .iter()
                    .map(|item| {
                        item["section"].as_str().map_or_else(
                            || settings.clone(),
                            |section| {
                                section
                                    .split('.')
                                    .fold(settings, |value, part| &value[part])
                                    .clone()
                            },
                        )
                    })
                    .collect()
            }))
        }
        "workspace/workspaceFolders" => folders.clone(),
        "workspace/applyEdit" => {
            json!({"applied": false, "failureReason": "Use hya edit tools and permissions to change files"})
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest" => Value::Null,
        _ => {
            return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unsupported server request: {method}")}});
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[cfg(all(test, unix))]
#[path = "transport_tests.rs"]
mod tests;
