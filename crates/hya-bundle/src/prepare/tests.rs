#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::error::BundleError;
use crate::model::{
    AgentRole, PreparedJsRuntimeMode, PreparedNativeCapability, PreparedProcessKind,
    PreparedRuntimeKind,
};
use crate::package::{inspect_public_package, write_public_package};
use crate::prepare::prepare_package;
use crate::source::{BundleSource, SourceFile};
use crate::{BundleCatalog, ExportKind};

#[test]
fn standardized_v2_js_process_bundle_prepares_and_round_trips() -> Result<(), BundleError> {
    let source = BundleSource::new(
        "v2-js",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity:
  id: acme/v2-js
  version: 1.0.0
  publisher: acme
runtime:
  kind: js
  mode: process
  entry: src/index.ts
  command: [bun, run, '${BUNDLE_ROOT}/src/index.ts']
  timeout_ms: 15000
resources:
  tools:
    - id: search
      path: tools/search.json
  hooks:
    - id: tool.execute.before
      path: hooks/hooks.json
  mcp:
    - id: remote-docs
      path: mcp/remote.json
  commands:
    - id: review
      path: commands/review.md
agents:
  - path: agents/scout.md
apis:
  - id: status
    method: GET
    scope: global
    path: /status
    response_schema: schemas/status.json
assets:
  - id: status-schema
    path: schemas/status.json
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"console.log('ok');\n".as_slice()),
            SourceFile::new(
                "tools/search.json",
                br#"{
  "schema_version": 1,
  "name": "search",
  "description": "Search indexed documents.",
  "permission": "read_only",
  "aliases": [{ "name": "find_docs", "visibility": "hidden" }],
  "parameters": { "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] },
  "entrypoint": { "js": "src/index.ts#search" }
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{
  "schema_version": 1,
  "hooks": [
    {
      "event": "tool.execute.before",
      "matcher": "^bash$",
      "handler": { "kind": "rpc", "method": "hook/tool.execute.before" },
      "failure_mode": "fail_closed",
      "timeout_ms": 5000
    }
  ]
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "mcp/remote.json",
                br#"{
  "transport": "http",
  "url": "https://mcp.example.com/v1",
  "headers": { "Authorization": "Bearer ${env:EXAMPLE_TOKEN}" },
  "timeout_ms": 30000
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "commands/review.md",
                b"---\nname: review\ndescription: Review changes\nhints: [diff]\nsubtask: false\n---\nReview $ARGUMENTS\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "agents/scout.md",
                b"---\nid: v2-scout\nrole: subagent\ndescription: Fast scout\nresource_view:\n  allow: [search, remote-docs]\n---\nYou are a fast scout.\n"
                    .as_slice(),
            ),
            SourceFile::new("schemas/status.json", br#"{"type":"object"}"#.as_slice()),
            // Undeclared dev test file must be ignored by deterministic pack.
            SourceFile::new("src/index.test.ts", b"test('x', () => {});\n".as_slice()),
        ],
    );

    let prepared = prepare_package(source.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("expected single bundle");
    };
    let runtime = bundle.runtime().expect("runtime preserved");
    assert_eq!(runtime.kind, PreparedRuntimeKind::Js);
    assert_eq!(runtime.mode, Some(PreparedJsRuntimeMode::Process));
    assert_eq!(runtime.entry.as_deref(), Some("src/index.ts"));

    let process = prepared
        .bundle_process("acme/v2-js")
        .expect("process derived from runtime");
    assert_eq!(process.kind, PreparedProcessKind::Bun);
    assert_eq!(process.timeout_ms, Some(15_000));

    assert_eq!(bundle.commands().len(), 1);
    assert_eq!(bundle.commands()[0].local_id, "review");
    assert_eq!(
        bundle.commands()[0].stable_id,
        "bundle:acme/v2-js/command/review"
    );

    assert_eq!(bundle.agents().len(), 1);
    assert_eq!(bundle.agents()[0].id.as_str(), "v2-scout");
    assert_eq!(bundle.agents()[0].role, AgentRole::Subagent);
    assert_eq!(
        bundle.agents()[0].prompt_source.as_deref(),
        Some("agents/scout.md")
    );
    assert_eq!(
        bundle.agents()[0].prompt.as_deref(),
        Some("You are a fast scout.")
    );

    let catalog = BundleCatalog::from_verified_catalogs(&[&prepared])?;
    assert!(
        catalog
            .resolve_resource("acme/v2-js", ExportKind::Command, "review")
            .is_ok()
    );

    // Deterministic archive includes commands, assets, entry, descriptors, and agent .md,
    // while omitting undeclared test files.
    let archive_a = write_public_package(&source)?;
    let archive_b = write_public_package(&source)?;
    assert_eq!(archive_a, archive_b);
    let inspected = inspect_public_package(&archive_a)?;
    assert_eq!(inspected.digest(), prepared.digest());
    Ok(())
}

#[test]
fn native_bundle_does_not_require_declaring_native_rust_sources() -> Result<(), BundleError> {
    let source = BundleSource::new(
        "v2-native",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity:
  id: acme/native-tools
  version: 1.0.0
  publisher: acme
runtime:
  kind: native
  abi_version: 1
  crate_dir: native
  library: native/libacme_native_tools.so
  capabilities: [tools, hooks]
resources:
  tools:
    - id: fetch
      path: tools/fetch.yaml
  hooks:
    - id: session.start
      path: hooks/hooks.json
"#
                .as_slice(),
            ),
            SourceFile::new(
                "tools/fetch.yaml",
                b"schema_version: 1\nname: fetch\ndescription: Fetch URL\npermission: read_only\nentrypoint:\n  native: fetch\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{"schema_version":1,"hooks":[{"event":"session.start","handler":{"kind":"native","target":"on_start"}}]}"#
                    .as_slice(),
            ),
            // Source Rust files exist on disk, NOT listed in bundle.yaml.
            SourceFile::new("native/Cargo.toml", b"[package]\nname = \"x\"\n".as_slice()),
            SourceFile::new("native/src/lib.rs", b"pub fn x() {}\n".as_slice()),
            SourceFile::new(
                "native/libacme_native_tools.so",
                vec![0x7f, b'E', b'L', b'F', 0, 1],
            ),
        ],
    );

    let prepared = prepare_package(source.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("expected single bundle");
    };
    let runtime = bundle.runtime().expect("native runtime");
    assert_eq!(runtime.kind, PreparedRuntimeKind::Native);
    assert_eq!(runtime.abi_version, Some(1));
    assert_eq!(
        runtime.capabilities,
        vec![
            PreparedNativeCapability::Tools,
            PreparedNativeCapability::Hooks,
        ]
    );
    let lib = bundle
        .extensions()
        .iter()
        .find(|res| res.stable_id == "bundle:acme/native-tools/library/runtime")
        .expect("packaged library resource");
    assert_eq!(lib.source_path, "native/libacme_native_tools.so");
    assert_eq!(
        lib.source_bytes().expect("decode binary"),
        vec![0x7f, b'E', b'L', b'F', 0, 1]
    );

    // Packing strips native/src/lib.rs and native/Cargo.toml cleanly.
    let bytes = write_public_package(&source)?;
    let inspected = inspect_public_package(&bytes)?;
    assert_eq!(inspected.digest(), prepared.digest());
    Ok(())
}

#[test]
fn rejects_conflicting_runtime_and_legacy_extensions_and_invalid_descriptors() {
    // 1. Conflicting runtime + extensions.process
    let conflict_process = BundleSource::new(
        "conflict-process",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Bundle
identity: { id: acme/conflict, version: 1.0.0, publisher: acme }
runtime: { kind: static }
extensions:
  process: { kind: bun, command: [bun, run, index.ts] }
"#
            .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(conflict_process),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("extensions.process")
    ));

    // 2. Conflicting runtime + extensions.libraries
    let conflict_libs = BundleSource::new(
        "conflict-libs",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
identity: { id: acme/conflict-lib, version: 1.0.0, publisher: acme }
runtime: { kind: native, abi_version: 1 }
extensions:
  libraries: [{ id: runtime, path: native/lib.so }]
"#
                .as_slice(),
            ),
            SourceFile::new("native/lib.so", vec![0, 1]),
        ],
    );
    assert!(matches!(
        prepare_package(conflict_libs),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("extensions.libraries")
    ));

    // 3. Unsupported schema_version
    let bad_schema = BundleSource::new(
        "bad-schema",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nschema_version: 99\nidentity: { id: acme/bad, version: 1.0.0, publisher: acme }\n"
                .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(bad_schema),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("schema_version")
    ));

    // 4. tool.execute.before with fail_open rejected
    let bad_hook = BundleSource::new(
        "bad-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
identity: { id: acme/bad-hook, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [bun, run, index.ts] }
resources:
  hooks: [{ id: tool.execute.before, path: hooks/hooks.json }]
assets: [{ id: index, path: index.ts }]
"#
                .as_slice(),
            ),
            SourceFile::new("index.ts", b"export {}\n".as_slice()),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{"schema_version":1,"hooks":[{"event":"tool.execute.before","handler":{"kind":"rpc","method":"hook/tool.execute.before"},"failure_mode":"fail_open"}]}"#
                    .as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_hook),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("fail_closed")
    ));

    // 5. MCP declaring both command and url rejected
    let bad_mcp = BundleSource::new(
        "bad-mcp",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nidentity: { id: acme/bad-mcp, version: 1.0.0, publisher: acme }\nresources:\n  mcp: [{ id: both, path: mcp/both.json }]\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "mcp/both.json",
                br#"{"command":["node","srv.js"],"url":"https://example.com"}"#.as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_mcp),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("not both")
    ));

    // 6. Tool descriptor name mismatch rejected
    let bad_tool = BundleSource::new(
        "bad-tool",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
identity: { id: acme/bad-tool, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: managed, entry: src/index.ts }
resources:
  tools: [{ id: search, path: tools/search.json }]
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"export {}\n".as_slice()),
            SourceFile::new(
                "tools/search.json",
                br#"{"schema_version":1,"name":"other","description":"Mismatch"}"#.as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_tool),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("declares name")
    ));
}

#[test]
fn supports_managed_js_static_runtime_claude_agent_md_and_multi_tool_yaml()
-> Result<(), BundleError> {
    // 1. Managed JS runtime + multi-tool tools.yaml + standalone agent with Claude Code `name:` & default role.
    let managed = BundleSource::new(
        "managed-js",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/managed, version: 1.0.0, publisher: acme }
runtime:
  kind: js
  mode: managed
  entry: src/index.ts
resources:
  tools:
    - { id: alpha, path: tools/tools.yaml }
    - { id: beta, path: tools/tools.yaml }
agents:
  - path: agents/helper.md
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"export const alpha = () => {};\n".as_slice()),
            SourceFile::new(
                "tools/tools.yaml",
                b"schema_version: 1\ntools:\n  - name: alpha\n    description: Alpha tool\n    permission: read_only\n  - name: beta\n    description: Beta tool\n    permission: tool\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "agents/helper.md",
                b"---\nname: claude-helper\ndescription: Claude-compatible agent\n---\nAssist the user.\n"
                    .as_slice(),
            ),
        ],
    );
    let prepared = prepare_package(managed.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("one bundle");
    };
    assert_eq!(
        bundle.runtime().unwrap().mode,
        Some(PreparedJsRuntimeMode::Managed)
    );
    assert!(prepared.bundle_process("acme/managed").is_none());
    assert_eq!(bundle.tools().len(), 2);
    assert_eq!(bundle.agents()[0].id.as_str(), "claude-helper");
    assert_eq!(bundle.agents()[0].role, AgentRole::Subagent);
    let packed = write_public_package(&managed)?;
    assert_eq!(inspect_public_package(&packed)?.digest(), prepared.digest());

    // 2. Explicit static runtime + stem-derived agent id.
    let static_bundle = BundleSource::new(
        "static-rt",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/static-rt, version: 1.0.0, publisher: acme }
runtime:
  kind: static
agents:
  - path: agents/reviewer.md
"#
                .as_slice(),
            ),
            SourceFile::new(
                "agents/reviewer.md",
                b"---\ndescription: Code reviewer\n---\nReview carefully.\n".as_slice(),
            ),
        ],
    );
    let static_prep = prepare_package(static_bundle)?;
    assert_eq!(
        static_prep.bundles()[0].runtime().unwrap().kind,
        PreparedRuntimeKind::Static
    );
    assert_eq!(static_prep.bundles()[0].agents()[0].id.as_str(), "reviewer");
    Ok(())
}
