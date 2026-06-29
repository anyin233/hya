use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{McpClient, McpError};

pub(crate) type ResourceMap = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceInfo {
    name: String,
    uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ResourceListResult {
    resources: Vec<ResourceInfo>,
}

pub(crate) async fn load(client: &McpClient, client_name: &str, timeout: Duration) -> ResourceMap {
    match client.call("resources/list", json!({}), timeout).await {
        Ok(value) => map(client_name, value).unwrap_or_default(),
        Err(error) => {
            tracing::debug!(%error, %client_name, "mcp resources unavailable");
            ResourceMap::new()
        }
    }
}

fn map(client_name: &str, value: Value) -> Result<ResourceMap, McpError> {
    let listed: ResourceListResult =
        serde_json::from_value(value).map_err(|e| McpError::Json(e.to_string()))?;
    listed
        .resources
        .into_iter()
        .map(|resource| keyed_resource(client_name, resource))
        .collect()
}

fn keyed_resource(client_name: &str, resource: ResourceInfo) -> Result<(String, Value), McpError> {
    let key = format!("{}:{}", sanitize(client_name), sanitize(&resource.name));
    let mut value = serde_json::to_value(resource).map_err(|e| McpError::Json(e.to_string()))?;
    if let Value::Object(ref mut object) = value {
        object.insert("client".to_string(), Value::String(client_name.to_string()));
    }
    Ok((key, value))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
