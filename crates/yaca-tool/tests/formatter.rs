#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use yaca_tool::{BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterProvider};

async fn status_json(config: FormatterConfig) -> serde_json::Value {
    let provider = BuiltinFormatterProvider::new(config);
    serde_json::to_value(provider.status(std::path::Path::new(".")).await.unwrap()).unwrap()
}

#[tokio::test]
async fn disabled_formatter_config_returns_empty_status() {
    let status = status_json(FormatterConfig::Disabled).await;

    assert_eq!(status, serde_json::json!([]));
}

#[tokio::test]
async fn builtin_formatter_config_returns_opencode_catalog_status() {
    let status = status_json(FormatterConfig::Builtins).await;

    assert!(status.as_array().unwrap().iter().any(|item| {
        item["name"] == "gofmt" && item["extensions"] == serde_json::json!([".go"])
    }));
    assert!(status.as_array().unwrap().iter().any(|item| {
        item["name"] == "prettier"
            && item["extensions"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(".ts"))
    }));
}

#[tokio::test]
async fn disabling_ruff_or_uv_disables_both_python_formatters() {
    let mut entries = BTreeMap::new();
    entries.insert(
        "ruff".to_string(),
        FormatterEntry {
            disabled: true,
            ..FormatterEntry::default()
        },
    );

    let status = status_json(FormatterConfig::Custom(entries)).await;

    assert!(
        !status
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "ruff")
    );
    assert!(
        !status
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "uv")
    );
}

#[tokio::test]
async fn custom_formatter_command_is_reported_enabled() {
    let mut entries = BTreeMap::new();
    entries.insert(
        "treefmt".to_string(),
        FormatterEntry {
            command: Some(vec!["treefmt".to_string(), "$FILE".to_string()]),
            extensions: Some(vec![".nix".to_string()]),
            ..FormatterEntry::default()
        },
    );

    let status = status_json(FormatterConfig::Custom(entries)).await;

    assert!(status.as_array().unwrap().iter().any(|item| {
        item == &serde_json::json!({
            "name": "treefmt",
            "extensions": [".nix"],
            "enabled": true
        })
    }));
}
