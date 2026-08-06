//! Model/agent/tool string newtypes + the canonical tool schema.

use serde::{Deserialize, Serialize};

macro_rules! str_newtype {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(
            /// Underlying string value for this name/ref.
            pub String,
        );

        impl $name {
            /// Wrap an owned or borrowed string as this id type.
            #[must_use]
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            /// Borrow the underlying string without allocation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

str_newtype!(
    AgentName,
    "Catalog or runtime agent name (builtin id, bundle agent, or user-defined name)."
);
str_newtype!(
    ModelRef,
    "Provider/model reference as configured (for example `provider/model` or a bare model id)."
);
str_newtype!(
    ToolName,
    "Canonical tool name as advertised to the model and used for dispatch."
);

/// Canonical, model-facing tool schema (the `llm` layer in design.md §4/§5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Function name the model must call.
    pub name: ToolName,
    /// Human/model-facing description of when and how to use the tool.
    pub description: String,
    /// JSON Schema for the tool's input object.
    pub input_schema: serde_json::Value,
    /// Optional JSON Schema for structured output; omit when free-form text/JSON is fine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}
