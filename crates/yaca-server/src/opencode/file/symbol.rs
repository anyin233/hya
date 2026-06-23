use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::Value;

use super::path::workdir;
use crate::ServerState;

#[derive(Deserialize)]
pub(super) struct FindSymbolQuery {
    query: String,
    directory: Option<String>,
}

pub(super) async fn find_symbol(
    State(st): State<ServerState>,
    Query(query): Query<FindSymbolQuery>,
    headers: HeaderMap,
) -> Json<Vec<Value>> {
    let root = workdir(&st, query.directory.as_deref(), &headers);
    let mut symbols = st
        .engine
        .lsp()
        .workspace_symbols(&root, query.query)
        .await
        .unwrap_or_default();
    symbols.retain(is_open_code_symbol_kind);
    symbols.truncate(10);
    Json(symbols)
}

fn is_open_code_symbol_kind(symbol: &Value) -> bool {
    matches!(
        symbol.get("kind").and_then(Value::as_u64),
        Some(5 | 6 | 10 | 11 | 12 | 13 | 14 | 23)
    )
}
