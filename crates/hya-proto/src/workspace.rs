use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAdapterInfo {
    #[serde(rename = "type")]
    pub r#type: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}
