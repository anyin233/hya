//! Integration tests for `hya-app`: formatter catalog status and execution.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::BuiltinFormatterProvider;
use hya_tool::{FormatterConfig, FormatterEntry, FormatterProvider};

fn tempdir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-fmt-{nanos}-{seq}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn status_json(config: FormatterConfig) -> serde_json::Value {
    let provider = BuiltinFormatterProvider::new(config);
    serde_json::to_value(provider.status(std::path::Path::new(".")).await.unwrap()).unwrap()
}

#[tokio::test]
async fn formatter_status_reflects_disabled_builtins_ruff_pair_and_custom_entries() {
    assert_eq!(
        status_json(FormatterConfig::Disabled).await,
        serde_json::json!([])
    );

    let builtins = status_json(FormatterConfig::Builtins).await;
    let list = builtins.as_array().unwrap();
    assert!(
        list.iter()
            .any(|i| i["name"] == "gofmt" && i["extensions"] == serde_json::json!([".go"]))
    );
    assert!(list.iter().any(|i| {
        i["name"] == "prettier"
            && i["extensions"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(".ts"))
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

    // Custom formatter entry is reported enabled.
    let status = status_json(FormatterConfig::Custom(BTreeMap::from([(
        "treefmt".to_string(),
        FormatterEntry {
            command: Some(vec!["treefmt".to_string(), "$FILE".to_string()]),
            extensions: Some(vec![".nix".to_string()]),
            ..FormatterEntry::default()
        },
    )])))
    .await;
    assert!(status.as_array().unwrap().iter().any(|item| {
        item == &serde_json::json!({"name": "treefmt", "extensions": [".nix"], "enabled": true})
    }));
}

#[tokio::test]
async fn custom_formatter_formats_matching_extension_and_surfaces_spawn_errors() {
    let dir = tempdir();
    let target = dir.join("note.txt");
    tokio::fs::write(&target, "raw\n").await.unwrap();
    let script = dir.join("formatter.sh");
    tokio::fs::write(&script, "printf '%s\\n' \"$FORMATTER_MARK\" > \"$1\"\n")
        .await
        .unwrap();

    let provider = BuiltinFormatterProvider::new(FormatterConfig::Custom(BTreeMap::from([(
        "textfmt".to_string(),
        FormatterEntry {
            command: Some(vec![
                "sh".into(),
                script.to_string_lossy().into_owned(),
                "$FILE".into(),
            ]),
            environment: BTreeMap::from([("FORMATTER_MARK".into(), "formatted".into())]),
            extensions: Some(vec![".txt".into()]),
            ..FormatterEntry::default()
        },
    )])));
    assert!(provider.format_file(&dir, &target).await.unwrap());
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "formatted\n"
    );

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
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_formatter_command_formats_matching_extension() {
    let dir = tempdir();
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
    let _ = std::fs::remove_dir_all(&dir);
}
