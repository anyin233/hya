//! Table-driven golden tests for hya's minimal `config.yaml` editors.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hya_app::config::{
    ModelEntryOverride, remove_model_entry, set_model_entry, upsert_oauth_provider,
    upsert_provider_entry,
};
use serde_norway::Value;

const FIXTURE: &str = "\
# hya config: hand-written
schema_version: 2
default_model: gw/plain   # the everyday model

# Providers I use
providers:
  gw:
    kind: openai            # switched last week
    base_url: \"https://gw.example/v1\"
    api_key: '{env:GW_KEY}'
    models:
      # cheap models first
      - plain               # fast
      - id: detailed
        name: Detailed      # shown in pickers
        # limits from the vendor docs
        limit:
          context: 1000
          output: 100
        reasoning: true

      - tail
  local:
    kind: openai-compatible
    base_url: http://127.0.0.1:8080/v1
    models: []    # discovered

# MCP servers
mcp: {}
system_note: |
  keep: this
  # not a comment
plugins: {}
permission:
  model: default
  rules: []
";

fn temp_config(yaml: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hya-cfg-minimal-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

fn edited(yaml: &str, op: impl Fn(&Path)) -> String {
    let path = temp_config(yaml);
    op(&path);
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    out
}

fn fixture_with(from: &str, to: &str) -> String {
    assert_eq!(FIXTURE.matches(from).count(), 1, "fragment must be unique");
    FIXTURE.replacen(from, to, 1)
}

fn parse(yaml: &str) -> Value {
    serde_norway::from_str(yaml).unwrap()
}

fn set(provider: &'static str, model: &'static str, patch: ModelEntryOverride) -> impl Fn(&Path) {
    move |path| set_model_entry(path, provider, model, &patch).unwrap()
}

fn name(value: &str) -> ModelEntryOverride {
    ModelEntryOverride {
        display_name: Some(value.to_string()),
        ..ModelEntryOverride::default()
    }
}

fn limits(context: Option<u32>, output: Option<u32>) -> ModelEntryOverride {
    ModelEntryOverride {
        context_limit: context,
        output_limit: output,
        ..ModelEntryOverride::default()
    }
}

#[test]
fn provider_upserts_preserve_comments_and_create_missing_sections() {
    assert_eq!(
        edited(FIXTURE, |p| {
            assert!(upsert_provider_entry(p, "fresh", "google", "https://g.example").unwrap());
        }),
        fixture_with(
            "    models: []    # discovered\n",
            "    models: []    # discovered\n  fresh:\n    kind: google\n    base_url: https://g.example\n    models: []\n",
        )
    );
    assert_eq!(
        edited(FIXTURE, |p| {
            assert!(
                !upsert_provider_entry(p, "gw", "anthropic", "https://new.example/v1").unwrap()
            );
        }),
        fixture_with(
            "    kind: openai            # switched last week\n    base_url: \"https://gw.example/v1\"\n",
            "    kind: anthropic            # switched last week\n    base_url: \"https://new.example/v1\"\n",
        )
    );
    assert_eq!(
        edited(
            "# my codex\nschema_version: 2\nproviders:\n  codex:   # oauth\n    kind: openai-codex\n    base_url: https://old.example/codex # stale\n\n# tail comment\n",
            |p| upsert_oauth_provider(
                p,
                "codex",
                "openai-codex",
                "https://chatgpt.com/backend-api/codex"
            )
            .unwrap(),
        ),
        "# my codex\nschema_version: 2\nproviders:\n  codex:   # oauth\n    kind: openai-codex\n    base_url: https://chatgpt.com/backend-api/codex # stale\n    models: []\n\n# tail comment\n"
    );
    for (input, expected) in [
        (
            "# only this\nschema_version: 2\ndefault_model: hya/offline\n",
            "# only this\nschema_version: 2\ndefault_model: hya/offline\nproviders:\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\n",
        ),
        (
            "schema_version: 2\ndefault_model: hya/offline\nproviders: {}   # none yet\nmcp: {}\n",
            "schema_version: 2\ndefault_model: hya/offline\nproviders:   # none yet\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\nmcp: {}\n",
        ),
    ] {
        assert_eq!(
            edited(input, |p| {
                upsert_provider_entry(p, "fresh", "openai", "https://x.example/v1").unwrap();
            }),
            expected
        );
    }

    let dir = temp_config("").parent().unwrap().to_path_buf();
    let nested = dir.join("nested/config.yaml");
    upsert_provider_entry(&nested, "fresh", "openai", "https://x.example/v1").unwrap();
    assert_eq!(
        std::fs::read_to_string(&nested).unwrap(),
        "schema_version: 2\ndefault_model: hya/offline\nproviders:\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\nmcp: {}\nplugins: {}\nbundles:\n  builtin:\n    profile: full\n    enable: []\n    disable: []\npermission:\n  model: default\n  rules: []\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn model_entry_edits_and_removals_patch_in_place() {
    type Case = (&'static str, Box<dyn Fn(&Path)>, String);
    let cases: Vec<Case> = vec![
        (
            "string->map",
            Box::new(set("gw", "plain", name("Plain"))),
            fixture_with(
                "      - plain               # fast\n",
                "      - id: plain               # fast\n        name: Plain\n",
            ),
        ),
        (
            "patch output limit",
            Box::new(set("gw", "detailed", limits(None, Some(200)))),
            fixture_with("          output: 100\n", "          output: 200\n"),
        ),
        (
            "patch name",
            Box::new(set("gw", "detailed", name("Deep"))),
            fixture_with(
                "        name: Detailed      # shown in pickers\n",
                "        name: Deep      # shown in pickers\n",
            ),
        ),
        (
            "clear name",
            Box::new(set("gw", "detailed", name("  "))),
            fixture_with("        name: Detailed      # shown in pickers\n", ""),
        ),
        (
            "clear all limits",
            Box::new(set("gw", "detailed", limits(Some(0), Some(0)))),
            fixture_with(
                "        limit:\n          context: 1000\n          output: 100\n",
                "",
            ),
        ),
        (
            "add limit map to string",
            Box::new(set("gw", "tail", limits(Some(500), None))),
            fixture_with(
                "      - tail\n",
                "      - id: tail\n        limit:\n          context: 500\n",
            ),
        ),
        (
            "append bare model",
            Box::new(set("gw", "vendor/new:free", ModelEntryOverride::default())),
            fixture_with("      - tail\n", "      - tail\n      - vendor/new:free\n"),
        ),
        (
            "empty flow list to block",
            Box::new(set("local", "m1", ModelEntryOverride::default())),
            fixture_with(
                "    models: []    # discovered\n",
                "    models:    # discovered\n      - m1\n",
            ),
        ),
        (
            "remove string model",
            Box::new(|p| assert!(remove_model_entry(p, "gw", "plain").unwrap())),
            fixture_with("      - plain               # fast\n", ""),
        ),
        (
            "remove map model",
            Box::new(|p| assert!(remove_model_entry(p, "gw", "detailed").unwrap())),
            fixture_with(
                "      - id: detailed\n        name: Detailed      # shown in pickers\n        # limits from the vendor docs\n        limit:\n          context: 1000\n          output: 100\n        reasoning: true\n",
                "",
            ),
        ),
    ];
    for (label, op, expected) in cases {
        assert_eq!(edited(FIXTURE, op), expected, "{label}");
    }

    // Map with only id collapses back to string; removing sole model leaves `models: []`.
    assert_eq!(
        edited(
            "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      # solo\n      - id: solo   # mine\n        name: Solo\n      - other\n",
            set("gw", "solo", name("")),
        ),
        "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      # solo\n      - solo   # mine\n      - other\n"
    );
    assert_eq!(
        edited(
            "schema_version: 2\nproviders:\n  gw:   # main\n    kind: openai\n    base_url: https://gw.example/v1\n    models:  # all\n      - only\n# end\n",
            |p| assert!(remove_model_entry(p, "gw", "only").unwrap()),
        ),
        "schema_version: 2\nproviders:\n  gw:   # main\n    kind: openai\n    base_url: https://gw.example/v1\n    models: []  # all\n# end\n"
    );
}

#[test]
fn formatting_styles_fallbacks_rejections_and_permissions() {
    // Compact sequence style preserved.
    let compact = "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - plain\n    - id: m\n      name: M\ndefault_model: gw/m\n";
    assert_eq!(
        edited(compact, set("gw", "plain", name("Plain"))),
        "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - id: plain\n      name: Plain\n    - id: m\n      name: M\ndefault_model: gw/m\n"
    );
    assert_eq!(
        edited(compact, set("gw", "new", ModelEntryOverride::default())),
        "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - plain\n    - id: m\n      name: M\n    - new\ndefault_model: gw/m\n"
    );

    // CRLF preserved.
    let crlf = FIXTURE.replace('\n', "\r\n");
    assert_eq!(
        edited(&crlf, set("gw", "plain", name("Plain"))),
        fixture_with(
            "      - plain               # fast\n",
            "      - id: plain               # fast\n        name: Plain\n",
        )
        .replace('\n', "\r\n")
    );

    // Flow-style provider rewritten locally in block style.
    assert_eq!(
        edited(
            "# keep me\nschema_version: 2\nproviders:\n  gw: {kind: openai, base_url: https://gw.example/v1, models: [a]}  # flow\n  other:\n    kind: google   # g\n    base_url: https://g.example\n    models: []\n",
            set("gw", "b", ModelEntryOverride::default()),
        ),
        "# keep me\nschema_version: 2\nproviders:\n  gw:  # flow\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - a\n      - b\n  other:\n    kind: google   # g\n    base_url: https://g.example\n    models: []\n"
    );

    // Anchors fall back to full semantic rewrite.
    let anchored = edited(
        "# lost on fallback\nschema_version: 2\ndefault_model: &dm gw/m\nlast_model: *dm\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models: [m]\n",
        set("gw", "n", ModelEntryOverride::default()),
    );
    assert!(!anchored.contains('#'));
    let val = parse(&anchored);
    assert_eq!(val["last_model"], Value::String("gw/m".into()));
    assert_eq!(val["providers"]["gw"]["models"], parse("[m, n]"));

    // Rejected edit leaves file untouched; valid edit preserves file mode.
    let path = temp_config(FIXTURE);
    assert!(set_model_entry(&path, "gw", "detailed", &limits(None, Some(5000))).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        set_model_entry(&path, "gw", "plain", &name("Plain")).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn every_minimal_edit_matches_full_render_semantically() {
    type EditOp = Box<dyn Fn(&Path)>;
    let canonical = serde_norway::to_string(&parse(FIXTURE)).unwrap();
    let ops: Vec<EditOp> = vec![
        Box::new(|p| {
            upsert_provider_entry(p, "fresh", "google", "https://g.example").unwrap();
        }),
        Box::new(|p| {
            upsert_provider_entry(p, "gw", "anthropic", "https://n.example").unwrap();
        }),
        Box::new(set("gw", "plain", name("Plain"))),
        Box::new(set("gw", "detailed", limits(Some(0), Some(0)))),
        Box::new(set("gw", "detailed", name(""))),
        Box::new(set(
            "gw",
            "tail",
            ModelEntryOverride {
                reasoning: Some(false),
                context_limit: Some(9000),
                ..ModelEntryOverride::default()
            },
        )),
        Box::new(set("local", "x", limits(None, Some(10)))),
        Box::new(|p| {
            remove_model_entry(p, "gw", "detailed").unwrap();
        }),
    ];
    for (index, op) in ops.iter().enumerate() {
        let minimal = edited(FIXTURE, op);
        let full = edited(&canonical, op);
        assert_eq!(parse(&minimal), parse(&full), "op {index}:\n{minimal}");
        assert!(minimal.contains("# MCP servers"), "op {index}");
    }
}
