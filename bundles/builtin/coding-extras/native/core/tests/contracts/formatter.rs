//! Formatter catalog status and spawn failures (custom execution and status
//! rows are covered by the crate's unit tests).

use std::collections::BTreeMap;
use std::path::Path;

use hya_coding_extras_core::BuiltinFormatterProvider;
use hya_coding_extras_core::formatter::{FormatterConfig, FormatterEntry, FormatterProvider};
use serde_json::{Value, json};

use crate::common::{TempDirGuard, tempdir};

async fn status_json(config: FormatterConfig) -> Value {
    let provider = BuiltinFormatterProvider::new(config);
    serde_json::to_value(provider.status(Path::new(".")).await.unwrap()).unwrap()
}

#[tokio::test]
async fn formatter_status_reflects_disabled_builtins_and_the_ruff_pair() {
    assert_eq!(status_json(FormatterConfig::Disabled).await, json!([]));

    let builtins = status_json(FormatterConfig::Builtins).await;
    let list = builtins.as_array().unwrap();
    assert!(
        list.iter()
            .any(|i| i["name"] == "gofmt" && i["extensions"] == json!([".go"]))
    );
    assert!(list.iter().any(|i| {
        i["name"] == "prettier" && i["extensions"].as_array().unwrap().contains(&json!(".ts"))
    }));

    // Disabling ruff disables both Python formatters (ruff + uv).
    let status = status_json(FormatterConfig::Custom(BTreeMap::from([(
        "ruff".to_string(),
        FormatterEntry {
            disabled: true,
            ..FormatterEntry::default()
        },
    )])))
    .await;
    let names: Vec<_> = status
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();
    assert!(!names.contains(&"ruff") && !names.contains(&"uv"));
}

#[tokio::test]
async fn custom_formatter_surfaces_spawn_errors() {
    let dir = tempdir("fmt-missing");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("note.txt");
    tokio::fs::write(&target, "raw\n").await.unwrap();
    let missing = BuiltinFormatterProvider::new(FormatterConfig::Custom(BTreeMap::from([(
        "textfmt".to_string(),
        FormatterEntry {
            command: Some(vec![
                dir.join("missing").to_string_lossy().into_owned(),
                "$FILE".into(),
            ]),
            extensions: Some(vec![".txt".into()]),
            ..FormatterEntry::default()
        },
    )])));
    assert!(missing.format_file(&dir, &target).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_formatter_command_formats_matching_extension() {
    let dir = tempdir("fmt-builtin");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(
        dir.join("composer.json"),
        r#"{"require-dev":{"laravel/pint":"^1"}}"#,
    )
    .await
    .unwrap();
    let bin_dir = dir.join("vendor/bin");
    tokio::fs::create_dir_all(&bin_dir).await.unwrap();
    std::os::unix::fs::symlink("/bin/sh", bin_dir.join("pint")).unwrap();
    let target = dir.join("app.php");
    tokio::fs::write(&target, "printf 'pint\\n' > \"$0\"\n")
        .await
        .unwrap();
    let provider = BuiltinFormatterProvider::new(FormatterConfig::Builtins);
    assert!(provider.format_file(&dir, &target).await.unwrap());
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "pint\n");
}
