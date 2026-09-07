//! Real stdio boundary regressions for versioning, isolation, and process ownership.
use super::*;
use crate::lsp::ProcessLspProvider;
use hya_tool::{LspOperation, LspProvider, LspRequest};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const SERVER: &str = r#"
import json, os, pathlib, re, subprocess, sys, time, urllib.parse
mode = sys.argv[1]
documents = {}
child = subprocess.Popen(['sleep', '60']) if mode in ('break', 'stall') else None

def send(value):
    data = json.dumps(value).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(data)}\r\n\r\n'.encode() + data)
    sys.stdout.buffer.flush()

def receive():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line: sys.exit(0)
        if line in (b'\r\n', b'\n'): break
        name, value = line.decode().split(':', 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))

def symbols(text, uri, query=''):
    return [{'name': name, 'kind': 12, 'location': {'uri': uri, 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}}}
            for name in re.findall(r'export function (\w+)', text) if query in name]

while True:
    request = receive()
    method = request.get('method')
    params = request.get('params', {})
    if method == 'initialize':
        root = pathlib.Path(urllib.parse.unquote(urllib.parse.urlparse(params['rootUri']).path))
        send({'jsonrpc': '2.0', 'id': 'folders', 'method': 'workspace/workspaceFolders', 'params': {}})
        folders = receive()
        assert folders.get('result', [{}])[0].get('uri') == params['rootUri'], folders
        result = {'capabilities': {'workspaceSymbolProvider': mode != 'limited', 'documentSymbolProvider': mode != 'limited', 'callHierarchyProvider': mode != 'limited' and isinstance(params.get('capabilities', {}).get('textDocument', {}).get('callHierarchy'), dict), 'textDocumentSync': 1}}
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        document = params['textDocument']
        uri, version = document['uri'], document['version']
        text = document.get('text', params.get('contentChanges', [{}])[0].get('text', ''))
        documents[uri] = text
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {'uri': uri, 'diagnostics': []}})
        time.sleep(0.08)
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {'uri': uri, 'version': version - 1, 'diagnostics': [{'severity': 1, 'message': 'STALE'}]}})
        rows = [{'severity': 1, 'message': text, 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}}] if 'BAD' in text else []
        published = {'uri': uri, 'diagnostics': rows}
        if 'unversioned' not in uri: published['version'] = version
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': published})
        continue
    elif method == 'textDocument/documentSymbol' and mode != 'limited':
        uri = params['textDocument']['uri']
        result = symbols(documents[uri], uri)
    elif method == 'textDocument/prepareCallHierarchy' and mode != 'limited':
        uri = params['textDocument']['uri']
        result = [{'name': row['name'], 'kind': row['kind'], 'uri': uri, 'range': row['location']['range'], 'selectionRange': row['location']['range']} for row in symbols(documents[uri], uri)]
    elif method == 'workspace/symbol' and mode != 'limited':
        result = [row for source in root.rglob('*.ts') for row in symbols(source.read_text(), source.as_uri(), params.get('query', ''))]
    elif method == 'test/pids':
        result = [os.getpid(), child.pid]
    elif method == 'test/break':
        sys.stdout.buffer.write(b'not a framed header\r\n\r\n')
        sys.stdout.buffer.flush()
        time.sleep(60)
        continue
    elif method == 'test/stall':
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': None})
        time.sleep(60)
        continue
    elif 'id' not in request:
        continue
    else:
        send({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32601, 'message': 'unsupported method'}})
        continue
    send({'jsonrpc': '2.0', 'id': request['id'], 'result': result})
"#;

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "hya-lsp-boundary-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(mode: &str) -> ServerConfig {
    ServerConfig {
        command: vec![
            "python3".into(),
            "-u".into(),
            "-c".into(),
            SERVER.into(),
            mode.into(),
        ],
        extensions: vec![".ts".into()],
        root_markers: vec![".git".into()],
        ..ServerConfig::default()
    }
}

fn provider(servers: BTreeMap<String, ServerConfig>) -> ProcessLspProvider {
    ProcessLspProvider {
        servers,
        clients: Mutex::new(BTreeMap::new()),
        updates: watch::channel(0).0,
    }
}

#[tokio::test]
async fn diagnostics_follow_document_versions_and_authorized_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new()?;
    let a = workspace.0.join("a");
    let b = workspace.0.join("b");
    std::fs::create_dir_all(&a)?;
    std::fs::create_dir_all(&b)?;
    let file_a = a.join("a#b.ts");
    let file_b = b.join("unversioned.ts");
    std::fs::write(&file_a, "export function original() {} // BAD old")?;
    std::fs::write(&file_b, "export function second() {} // BAD delayed")?;
    let provider = provider(BTreeMap::from([("fixture".into(), config("full"))]));
    provider.touch_file(&file_a, "document").await?;
    let first = provider.diagnostics(&a, &[]).await?;
    assert!(
        first[&file_a.to_string_lossy().into_owned()][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("BAD old"))
    );
    std::fs::write(&file_a, "export function fixed() {}")?;
    provider.touch_file(&file_a, "document").await?;
    let fixed = provider.diagnostics(&a, &[]).await?;
    assert_eq!(fixed[&file_a.to_string_lossy().into_owned()], json!([]));
    let symbols = provider
        .execute(LspRequest {
            operation: LspOperation::DocumentSymbol,
            file: file_a.clone(),
            uri: format!("file://{}", file_a.display()),
            line: 0,
            character: 0,
            query: None,
        })
        .await?;
    assert!(symbols.iter().any(|row| row["name"] == "fixed"));
    provider.touch_file(&file_b, "document").await?;
    let second = provider.diagnostics(&b, &[]).await?;
    assert!(second.get(file_a.to_string_lossy().as_ref()).is_none());
    assert!(
        second[&file_b.to_string_lossy().into_owned()][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("BAD delayed"))
    );
    let authorized = provider.diagnostics(&b, &[&file_a]).await?;
    assert_eq!(
        authorized[&file_a.to_string_lossy().into_owned()],
        json!([])
    );
    Ok(())
}

#[tokio::test]
async fn capable_workspace_client_survives_unsupported_sibling()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new()?;
    std::fs::create_dir_all(workspace.0.join(".git"))?;
    std::fs::create_dir_all(workspace.0.join("src"))?;
    let file = workspace.0.join("src/main.ts");
    std::fs::write(&file, "export function nestedSymbol() {}")?;
    let provider = provider(BTreeMap::from([
        ("a-limited".into(), config("limited")),
        ("b-full".into(), config("full")),
    ]));
    let symbols = provider
        .execute(LspRequest {
            operation: LspOperation::DocumentSymbol,
            file: file.clone(),
            uri: file_uri(&file)?,
            line: 0,
            character: 0,
            query: None,
        })
        .await?;
    assert!(symbols.iter().any(|row| row["name"] == "nestedSymbol"));
    let symbols = provider
        .execute(LspRequest {
            operation: LspOperation::WorkspaceSymbol,
            file: workspace.0.clone(),
            uri: file_uri(&workspace.0)?,
            line: 0,
            character: 0,
            query: Some("nestedSymbol".into()),
        })
        .await?;
    assert!(symbols.iter().any(|row| row["name"] == "nestedSymbol"));
    let hierarchy = provider
        .execute(LspRequest {
            operation: LspOperation::PrepareCallHierarchy,
            file: file.clone(),
            uri: file_uri(&file)?,
            line: 0,
            character: 16,
            query: None,
        })
        .await?;
    assert!(hierarchy.iter().any(|row| row["name"] == "nestedSymbol"));
    Ok(())
}

#[cfg(target_os = "linux")]
async fn wait_stopped(pids: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let pids = pids.as_array().ok_or("missing process ids")?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut running = false;
            for pid in pids {
                if let Some(pid) = pid.as_u64()
                    && let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                {
                    running |= stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, rest)| !rest.starts_with("Z "));
                }
            }
            if !running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn malformed_transport_reaps_cached_server_and_descendants()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new()?;
    let (updates, mut changed) = watch::channel(0);
    let client = Client::start(&config("break"), &workspace.0, updates).await?;
    let pids = client.request("test/pids", json!({})).await?;
    assert!(client.request("test/break", json!({})).await.is_err());
    tokio::time::timeout(Duration::from_secs(3), changed.changed()).await??;
    assert!(client.failure().is_some());
    wait_stopped(&pids).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelled_partial_write_poison_closes_server_tree()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new()?;
    let client = Client::start(&config("stall"), &workspace.0, watch::channel(0).0).await?;
    let pids = client.request("test/pids", json!({})).await?;
    client.request("test/stall", json!({})).await?;
    let writing = client.clone();
    let task = tokio::spawn(async move {
        writing
            .notify(
                "textDocument/didOpen",
                json!({"text": "x".repeat(8 * 1024 * 1024)}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while client.shared.writer.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    task.abort();
    let _ = task.await;
    assert!(client.failure().is_some());
    assert!(client.notify("initialized", json!({})).await.is_err());
    wait_stopped(&pids).await?;
    Ok(())
}
