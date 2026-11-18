//! Golden tests for hya's `config.yaml` writers: provider upsert (also the
//! OAuth login writer), model entry set/patch, and model entry removal change
//! only the affected lines and keep the user's comments, blank lines, key
//! order, quoting, and indentation everywhere else. Structures the minimal
//! editor cannot handle safely fall back to a full re-render.

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

/// Run `op` on a file holding `yaml` and return the resulting text.
fn edited(yaml: &str, op: impl Fn(&Path)) -> String {
    let path = temp_config(yaml);
    op(&path);
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    out
}

/// `FIXTURE` with exactly one occurrence of `from` replaced by `to`.
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
fn new_provider_is_appended_at_the_end_of_providers() {
    let out = edited(FIXTURE, |path| {
        assert!(upsert_provider_entry(path, "fresh", "google", "https://g.example").unwrap());
    });
    let expected = fixture_with(
        "    models: []    # discovered\n",
        "    models: []    # discovered\n  fresh:\n    kind: google\n    base_url: https://g.example\n    models: []\n",
    );
    assert_eq!(out, expected);
}

#[test]
fn existing_provider_kind_and_base_url_change_in_place() {
    let out = edited(FIXTURE, |path| {
        assert!(!upsert_provider_entry(path, "gw", "anthropic", "https://new.example/v1").unwrap());
    });
    let expected = fixture_with(
        "    kind: openai            # switched last week\n    base_url: \"https://gw.example/v1\"\n",
        "    kind: anthropic            # switched last week\n    base_url: \"https://new.example/v1\"\n",
    );
    assert_eq!(out, expected);
}

#[test]
fn oauth_upsert_keeps_comments_and_adds_missing_models_key() {
    let yaml = "# my codex\nproviders:\n  codex:   # oauth\n    kind: openai-codex\n    base_url: https://old.example/codex # stale\n\n# tail comment\n";
    let out = edited(yaml, |path| {
        upsert_oauth_provider(
            path,
            "codex",
            "openai-codex",
            "https://chatgpt.com/backend-api/codex",
        )
        .unwrap();
    });
    assert_eq!(
        out,
        "# my codex\nproviders:\n  codex:   # oauth\n    kind: openai-codex\n    base_url: https://chatgpt.com/backend-api/codex # stale\n    models: []\n\n# tail comment\n"
    );
}

#[test]
fn providers_key_is_created_when_missing_or_empty_flow() {
    let out = edited("# only this\ndefault_model: hya/offline\n", |path| {
        upsert_provider_entry(path, "fresh", "openai", "https://x.example/v1").unwrap();
    });
    assert_eq!(
        out,
        "# only this\ndefault_model: hya/offline\nproviders:\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\n"
    );

    let out = edited(
        "default_model: hya/offline\nproviders: {}   # none yet\nmcp: {}\n",
        |path| {
            upsert_provider_entry(path, "fresh", "openai", "https://x.example/v1").unwrap();
        },
    );
    assert_eq!(
        out,
        "default_model: hya/offline\nproviders:   # none yet\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\nmcp: {}\n"
    );
}

#[test]
fn missing_file_is_created_from_the_default_document() {
    let dir = temp_config("").parent().unwrap().to_path_buf();
    let path = dir.join("nested/config.yaml");
    upsert_provider_entry(&path, "fresh", "openai", "https://x.example/v1").unwrap();
    let out = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        out,
        "default_model: hya/offline\nproviders:\n  fresh:\n    kind: openai\n    base_url: https://x.example/v1\n    models: []\nmcp: {}\nplugins: {}\npermission:\n  model: default\n  rules: []\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn string_entry_becomes_a_mapping_in_place() {
    let out = edited(FIXTURE, set("gw", "plain", name("Plain")));
    let expected = fixture_with(
        "      - plain               # fast\n",
        "      - id: plain               # fast\n        name: Plain\n",
    );
    assert_eq!(out, expected);
}

#[test]
fn one_field_inside_a_model_map_is_patched_in_place() {
    let out = edited(FIXTURE, set("gw", "detailed", limits(None, Some(200))));
    assert_eq!(
        out,
        fixture_with("          output: 100\n", "          output: 200\n")
    );

    let out = edited(FIXTURE, set("gw", "detailed", name("Deep")));
    assert_eq!(
        out,
        fixture_with(
            "        name: Detailed      # shown in pickers\n",
            "        name: Deep      # shown in pickers\n"
        )
    );
}

#[test]
fn clearing_a_field_removes_only_its_lines() {
    let out = edited(FIXTURE, set("gw", "detailed", name("  ")));
    assert_eq!(
        out,
        fixture_with("        name: Detailed      # shown in pickers\n", "")
    );
}

#[test]
fn clearing_every_limit_drops_the_limit_map() {
    let out = edited(FIXTURE, set("gw", "detailed", limits(Some(0), Some(0))));
    assert_eq!(
        out,
        fixture_with(
            "        limit:\n          context: 1000\n          output: 100\n",
            ""
        )
    );
}

#[test]
fn limit_map_is_created_for_a_string_entry() {
    let out = edited(FIXTURE, set("gw", "tail", limits(Some(500), None)));
    assert_eq!(
        out,
        fixture_with(
            "      - tail\n",
            "      - id: tail\n        limit:\n          context: 500\n"
        )
    );
}

#[test]
fn a_new_model_is_appended_after_the_last_entry() {
    let out = edited(
        FIXTURE,
        set("gw", "vendor/new:free", ModelEntryOverride::default()),
    );
    assert_eq!(
        out,
        fixture_with("      - tail\n", "      - tail\n      - vendor/new:free\n")
    );

    // An empty flow list becomes a block list; its inline comment stays.
    let out = edited(FIXTURE, set("local", "m1", ModelEntryOverride::default()));
    assert_eq!(
        out,
        fixture_with(
            "    models: []    # discovered\n",
            "    models:    # discovered\n      - m1\n"
        )
    );
}

#[test]
fn map_entry_turns_back_into_a_string_when_only_id_remains() {
    let yaml = "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      # solo\n      - id: solo   # mine\n        name: Solo\n      - other\n";
    let out = edited(yaml, set("gw", "solo", name("")));
    assert_eq!(
        out,
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      # solo\n      - solo   # mine\n      - other\n"
    );
}

#[test]
fn removing_a_model_removes_only_its_own_lines() {
    let out = edited(FIXTURE, |path| {
        assert!(remove_model_entry(path, "gw", "plain").unwrap());
    });
    assert_eq!(
        out,
        fixture_with("      - plain               # fast\n", "")
    );

    let out = edited(FIXTURE, |path| {
        assert!(remove_model_entry(path, "gw", "detailed").unwrap());
    });
    assert_eq!(
        out,
        fixture_with(
            "      - id: detailed\n        name: Detailed      # shown in pickers\n        # limits from the vendor docs\n        limit:\n          context: 1000\n          output: 100\n        reasoning: true\n",
            ""
        )
    );

    // Removing the only entry leaves an empty list.
    let yaml = "providers:\n  gw:   # main\n    kind: openai\n    base_url: https://gw.example/v1\n    models:  # all\n      - only\n# end\n";
    let out = edited(yaml, |path| {
        assert!(remove_model_entry(path, "gw", "only").unwrap());
    });
    assert_eq!(
        out,
        "providers:\n  gw:   # main\n    kind: openai\n    base_url: https://gw.example/v1\n    models: []  # all\n# end\n"
    );
}

#[test]
fn compact_sequences_written_by_older_hya_are_edited_in_their_own_style() {
    let yaml = "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - plain\n    - id: m\n      name: M\ndefault_model: gw/m\n";
    let out = edited(yaml, set("gw", "plain", name("Plain")));
    assert_eq!(
        out,
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - id: plain\n      name: Plain\n    - id: m\n      name: M\ndefault_model: gw/m\n"
    );
    let out = edited(yaml, set("gw", "new", ModelEntryOverride::default()));
    assert_eq!(
        out,
        "providers:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n    - plain\n    - id: m\n      name: M\n    - new\ndefault_model: gw/m\n"
    );
}

#[test]
fn crlf_files_keep_crlf_line_endings() {
    let crlf = FIXTURE.replace('\n', "\r\n");
    let out = edited(&crlf, set("gw", "plain", name("Plain")));
    let expected = fixture_with(
        "      - plain               # fast\n",
        "      - id: plain               # fast\n        name: Plain\n",
    )
    .replace('\n', "\r\n");
    assert_eq!(out, expected);
}

#[test]
fn flow_style_provider_is_rewritten_locally_in_block_style() {
    let yaml = "# keep me\nproviders:\n  gw: {kind: openai, base_url: https://gw.example/v1, models: [a]}  # flow\n  other:\n    kind: google   # g\n    base_url: https://g.example\n    models: []\n";
    let out = edited(yaml, set("gw", "b", ModelEntryOverride::default()));
    assert_eq!(
        out,
        "# keep me\nproviders:\n  gw:  # flow\n    kind: openai\n    base_url: https://gw.example/v1\n    models:\n      - a\n      - b\n  other:\n    kind: google   # g\n    base_url: https://g.example\n    models: []\n"
    );
}

#[test]
fn anchors_fall_back_to_a_full_rewrite_with_the_same_value() {
    let yaml = "# lost on fallback\ndefault_model: &dm gw/m\nlast_model: *dm\nproviders:\n  gw:\n    kind: openai\n    base_url: https://gw.example/v1\n    models: [m]\n";
    let out = edited(yaml, set("gw", "n", ModelEntryOverride::default()));
    assert!(!out.contains('#'), "full rewrite drops comments: {out}");
    let value = parse(&out);
    assert_eq!(out, serde_norway::to_string(&value).unwrap());
    assert_eq!(value["last_model"], Value::String("gw/m".into()));
    assert_eq!(value["providers"]["gw"]["models"], parse("[m, n]"), "{out}");
}

#[test]
fn every_minimal_edit_matches_the_full_render_semantically() {
    // The same operation on a comment-free canonical copy (the old writer's
    // output style) must yield the same parsed document.
    let canonical = serde_norway::to_string(&parse(FIXTURE)).unwrap();
    type Op = Box<dyn Fn(&Path)>;
    let ops: Vec<Op> = vec![
        Box::new(|path| {
            upsert_provider_entry(path, "fresh", "google", "https://g.example").unwrap();
        }),
        Box::new(|path| {
            upsert_provider_entry(path, "gw", "anthropic", "https://n.example").unwrap();
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
        Box::new(|path| {
            remove_model_entry(path, "gw", "detailed").unwrap();
        }),
    ];
    for (index, op) in ops.iter().enumerate() {
        let minimal = edited(FIXTURE, op);
        let full = edited(&canonical, op);
        assert_eq!(parse(&minimal), parse(&full), "op {index}:\n{minimal}");
        assert!(minimal.contains("# MCP servers"), "op {index}: {minimal}");
    }
}

#[test]
fn rejected_edits_leave_the_file_byte_for_byte_untouched() {
    let path = temp_config(FIXTURE);
    let result = set_model_entry(&path, "gw", "detailed", &limits(None, Some(5000)));
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[cfg(unix)]
#[test]
fn file_mode_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp_config(FIXTURE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    set_model_entry(&path, "gw", "plain", &name("Plain")).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
