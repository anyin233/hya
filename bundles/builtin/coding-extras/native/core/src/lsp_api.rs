//! [`NativeBundleApiHandler`] for `hya/coding-extras`: workspace symbol search
//! through the bundle's configured language servers.
//!
//! `GET /v2/bundles/hya%2Fcoding-extras/api/lsp/symbols?directory=<abs>&query=<q>&limit=<n>`
//! answers `{"symbols": [{"path", "name", "kind", "lineNumber"}]}`.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use hya_runtime_abi::{
    ApiMethod, ApiScope, HostError, NativeApiEndpoint, NativeBundleApiHandler,
    NativeBundleApiReply, NativeBundleApiRequest,
};
use hya_tool::path::display_path;
use serde_json::{Value, json};

use crate::lsp_service::uri_to_file_path;
use crate::service::{BUNDLE_ID, CodingExtrasState};

/// Symbols returned when the request sets no `limit` (or `0`).
const DEFAULT_LIMIT: usize = 10;
/// Largest accepted `limit`.
const MAX_LIMIT: usize = 100;

static ENDPOINTS: LazyLock<[NativeApiEndpoint; 1]> = LazyLock::new(|| {
    [NativeApiEndpoint {
        id: "search_symbols".to_owned(),
        method: ApiMethod::Get,
        scope: ApiScope::Global,
        path: "/lsp/symbols".to_owned(),
        description: "Search workspace symbols under a directory through the configured \
                      language servers."
            .to_owned(),
    }]
});

/// `BundleApi` handler serving `GET /lsp/symbols` from the bundle's LSP state.
pub struct LspApi {
    state: Arc<CodingExtrasState>,
}

impl LspApi {
    /// Serve symbol search from `state`'s language servers.
    #[must_use]
    pub fn new(state: Arc<CodingExtrasState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl NativeBundleApiHandler for LspApi {
    fn id(&self) -> &str {
        BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        ENDPOINTS.as_slice()
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let directory = request
            .query
            .get("directory")
            .map(|value| value.trim())
            .unwrap_or_default();
        if !Path::new(directory).is_absolute() {
            return Ok(invalid_argument(format!(
                "directory must be an absolute path, got `{directory}`"
            )));
        }
        let limit = match request.query.get("limit").map(|raw| raw.parse::<usize>()) {
            None | Some(Ok(0)) => DEFAULT_LIMIT,
            Some(Ok(limit)) => limit.min(MAX_LIMIT),
            Some(Err(error)) => return Ok(invalid_argument(format!("invalid limit: {error}"))),
        };
        let query = request.query.get("query").cloned().unwrap_or_default();
        let root = Path::new(directory);
        let rows = match self.state.lsp().workspace_symbols(root, query).await {
            Ok(rows) => rows,
            Err(error) => {
                return Ok(NativeBundleApiReply {
                    status: 502,
                    body: json!({ "error": { "code": "lsp_failed", "message": error.to_string() } }),
                });
            }
        };
        let symbols: Vec<Value> = rows
            .iter()
            .take(limit)
            .map(|row| symbol(root, row))
            .collect();
        Ok(NativeBundleApiReply {
            status: 200,
            body: json!({ "symbols": symbols }),
        })
    }
}

fn invalid_argument(message: String) -> NativeBundleApiReply {
    NativeBundleApiReply {
        status: 400,
        body: json!({ "error": { "code": "invalid_argument", "message": message } }),
    }
}

/// One LSP `SymbolInformation`/`WorkspaceSymbol` row in the response shape:
/// `path` relative to `root`, 1-based `lineNumber` (0 when the server sent no
/// range).
fn symbol(root: &Path, row: &Value) -> Value {
    let location = row.get("location");
    let path = location
        .and_then(|location| location.get("uri"))
        .and_then(Value::as_str)
        .and_then(uri_to_file_path)
        .map(|file| display_path(file.strip_prefix(root).unwrap_or(&file)))
        .unwrap_or_default();
    let line_number = location
        .and_then(|location| location.pointer("/range/start/line"))
        .and_then(Value::as_u64)
        .map_or(0, |line| line + 1);
    json!({
        "path": path,
        "name": row.get("name").and_then(Value::as_str).unwrap_or_default(),
        "kind": symbol_kind(row.get("kind").and_then(Value::as_u64)),
        "lineNumber": line_number,
    })
}

/// Collapse an LSP `SymbolKind` number into the four response kinds.
fn symbol_kind(kind: Option<u64>) -> &'static str {
    match kind {
        // Method, Constructor, Function.
        Some(6 | 9 | 12) => "SYMBOL_KIND_FUNCTION",
        // Class, Enum, Interface, Struct, TypeParameter.
        Some(5 | 10 | 11 | 23 | 26) => "SYMBOL_KIND_TYPE",
        // Module, Namespace, Package.
        Some(2..=4) => "SYMBOL_KIND_MODULE",
        _ => "SYMBOL_KIND_OTHER",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use hya_tool::{FormatterPlane, LspError, LspOperation, LspPlane, LspProvider, LspRequest};

    use super::*;

    struct SymbolLsp {
        rows: Vec<Value>,
        requests: Arc<Mutex<Vec<LspRequest>>>,
    }

    #[async_trait]
    impl LspProvider for SymbolLsp {
        async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
            Ok(true)
        }

        async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.rows.clone())
        }
    }

    fn get(query: &[(&str, &str)]) -> NativeBundleApiRequest {
        NativeBundleApiRequest {
            api: "search_symbols".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Global,
            path: "/lsp/symbols".into(),
            path_params: BTreeMap::new(),
            query: query
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            body: Value::Null,
            session: None,
        }
    }

    #[tokio::test]
    async fn search_symbols_maps_lsp_rows_under_the_directory() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let row = |name: &str, kind: u64, line: u64| {
            json!({
                "name": name,
                "kind": kind,
                "location": {
                    "uri": "file:///work/src/main.ts",
                    "range": { "start": { "line": line, "character": 0 } }
                }
            })
        };
        let api = LspApi::new(Arc::new(CodingExtrasState::with_planes(
            FormatterPlane::default(),
            LspPlane::new(Arc::new(SymbolLsp {
                rows: vec![row("qaSymbol", 12, 4), row("Shape", 5, 9)],
                requests: Arc::clone(&requests),
            })),
        )));
        assert_eq!(api.endpoints()[0].path, "/lsp/symbols");

        let reply = api
            .handle(get(&[("directory", "/work"), ("query", "qa")]))
            .await
            .unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.body,
            json!({ "symbols": [
                { "path": "src/main.ts", "name": "qaSymbol", "kind": "SYMBOL_KIND_FUNCTION", "lineNumber": 5 },
                { "path": "src/main.ts", "name": "Shape", "kind": "SYMBOL_KIND_TYPE", "lineNumber": 10 },
            ]})
        );
        {
            let calls = requests.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].operation, LspOperation::WorkspaceSymbol);
            assert_eq!(calls[0].file, PathBuf::from("/work"));
            assert_eq!(calls[0].query.as_deref(), Some("qa"));
        }

        let limited = api
            .handle(get(&[("directory", "/work"), ("limit", "1")]))
            .await
            .unwrap();
        assert_eq!(limited.body["symbols"].as_array().unwrap().len(), 1);

        for bad in [
            get(&[("query", "qa")]),
            get(&[("directory", "work")]),
            get(&[("directory", "/work"), ("limit", "many")]),
        ] {
            let reply = api.handle(bad).await.unwrap();
            assert_eq!(reply.status, 400);
            assert_eq!(reply.body["error"]["code"], "invalid_argument");
        }
    }

    #[tokio::test]
    async fn search_symbols_without_a_language_server_is_empty() {
        let api = LspApi::new(Arc::new(CodingExtrasState::new()));
        let reply = api.handle(get(&[("directory", "/work")])).await.unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, json!({ "symbols": [] }));
    }
}
