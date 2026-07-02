#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::{BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterProvider};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("hya-formatter-test-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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
async fn builtin_formatter_config_returns_compat_catalog_status() {
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

#[tokio::test]
async fn custom_formatter_command_formats_matching_extension() {
    // Given: a custom formatter command with an environment override.
    let dir = tempdir();
    let target = dir.join("note.txt");
    tokio::fs::write(&target, "raw\n").await.unwrap();
    let script = dir.join("formatter.sh");
    tokio::fs::write(&script, "printf '%s\\n' \"$FORMATTER_MARK\" > \"$1\"\n")
        .await
        .unwrap();

    let mut environment = BTreeMap::new();
    environment.insert("FORMATTER_MARK".to_string(), "formatted".to_string());
    let mut entries = BTreeMap::new();
    entries.insert(
        "textfmt".to_string(),
        FormatterEntry {
            command: Some(vec![
                "sh".to_string(),
                script.to_string_lossy().into_owned(),
                "$FILE".to_string(),
            ]),
            environment,
            extensions: Some(vec![".txt".to_string()]),
            ..FormatterEntry::default()
        },
    );
    let provider = BuiltinFormatterProvider::new(FormatterConfig::Custom(entries));

    // When: formatting a file with the matching extension.
    let formatted = provider.format_file(&dir, &target).await.unwrap();

    // Then: the formatter ran and rewrote the file.
    assert!(formatted);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "formatted\n"
    );
}

#[tokio::test]
async fn custom_formatter_command_failure_returns_error() {
    // Given: a custom formatter whose command cannot be spawned.
    let dir = tempdir();
    let target = dir.join("note.txt");
    tokio::fs::write(&target, "raw\n").await.unwrap();

    let mut entries = BTreeMap::new();
    entries.insert(
        "textfmt".to_string(),
        FormatterEntry {
            command: Some(vec![
                dir.join("missing-formatter").to_string_lossy().into_owned(),
                "$FILE".to_string(),
            ]),
            extensions: Some(vec![".txt".to_string()]),
            ..FormatterEntry::default()
        },
    );
    let provider = BuiltinFormatterProvider::new(FormatterConfig::Custom(entries));

    // When: formatting a file with the matching extension.
    let result = provider.format_file(&dir, &target).await;

    // Then: spawn failures surface as formatter errors.
    assert!(result.is_err(), "expected formatter error, got {result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_formatter_command_formats_matching_extension() {
    // Given: a built-in formatter enabled by its Compat project probe.
    let dir = tempdir();
    tokio::fs::write(
        dir.join("composer.json"),
        r#"{"require-dev":{"laravel/pint":"^1"}}"#,
    )
    .await
    .unwrap();
    let bin_dir = dir.join("vendor/bin");
    tokio::fs::create_dir_all(&bin_dir).await.unwrap();
    let pint = bin_dir.join("pint");
    tokio::fs::write(&pint, "#!/bin/sh\nprintf 'pint\\n' > \"$1\"\n")
        .await
        .unwrap();
    std::fs::set_permissions(&pint, std::fs::Permissions::from_mode(0o755)).unwrap();
    let target = dir.join("app.php");
    tokio::fs::write(&target, "raw\n").await.unwrap();
    let provider = BuiltinFormatterProvider::new(FormatterConfig::Builtins);

    // When: formatting a matching PHP file.
    let formatted = provider.format_file(&dir, &target).await.unwrap();

    // Then: hya executes the Compat built-in command.
    assert!(formatted);
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "pint\n");
}
