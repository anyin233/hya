//! First-party bundles, `hya-extra/*` distribution bundles, and repository documentation examples.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use hya_bundle::{
    AgentRole, BundleError, BundleSource, FIRST_PARTY_BUNDLES, FirstPartySource,
    PreparedBundleKind, PreparedCatalog, SourceFile, first_party_bundle, first_party_source,
    first_party_source_root, inspect_public_package, load_first_party, prepare_package,
    write_public_package,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hya-first-party-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("hya-bundle must live under <repository>/crates")
}

fn stage_package(directory: &Path, identity: &str, source_identity: &str) -> PathBuf {
    let name = source_identity.strip_prefix("hya/").expect("hya identity");
    let source = BundleSource::read_directory(first_party_source_root().join("presets").join(name))
        .expect("read in-tree source");
    std::fs::create_dir_all(directory).expect("create bundles directory");
    let package = directory.join(format!(
        "hya-{}.hyabundle",
        identity.strip_prefix("hya/").expect("hya identity")
    ));
    std::fs::write(
        &package,
        write_public_package(&source).expect("write public package"),
    )
    .expect("write package");
    package
}

fn extra_bundle_dirs() -> Vec<PathBuf> {
    let root = first_party_source_root().join("extra");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn workflow_doc_paths() -> Vec<PathBuf> {
    let root = repository_root();
    let mut paths = vec![root.join("docs/workflows.md")];
    if root.join(".autors/hya/wiki").is_dir() {
        paths.push(root.join(".autors/hya/wiki/pages/architecture/workflow-composition.md"));
    }
    paths
}

fn prepare_bun_example(directory: &str) -> (PreparedCatalog, PreparedCatalog) {
    let path = repository_root().join("docs/examples").join(directory);
    let source = BundleSource::read_directory(&path).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must exist: {error}",
            path.display()
        )
    });
    let first = prepare_package(source.clone())
        .unwrap_or_else(|error| panic!("Bun example {} must prepare: {error:?}", path.display()));
    let second = prepare_package(source).unwrap_or_else(|error| {
        panic!(
            "Bun example {} must be deterministic: {error:?}",
            path.display()
        )
    });
    (first, second)
}

fn fenced_block(doc: &str, heading: &str, language: &str, index: usize) -> String {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("heading `{heading}` must exist"));
    let fence = format!("```{language}\n");
    let mut rest = &doc[start..];
    for _ in 0..index {
        let skip = rest.find(&fence).expect("missing block");
        rest = &rest[skip + fence.len()..];
    }
    let open = rest
        .find(&fence)
        .unwrap_or_else(|| panic!("no ```{language} block after `{heading}`"));
    let body = &rest[open + fence.len()..];
    let close = body
        .find("\n```")
        .unwrap_or_else(|| panic!("unterminated ```{language} block after `{heading}`"));
    body[..=close].to_string()
}

fn authoring_doc() -> String {
    std::fs::read_to_string(repository_root().join("docs/agent-bundle-authoring.md"))
        .expect("docs/agent-bundle-authoring.md must exist")
}

#[test]
fn allowlist_names_every_in_tree_first_party_source() {
    for identity in FIRST_PARTY_BUNDLES {
        let source = first_party_source(Path::new("/nowhere/target/debug"), identity);
        assert!(
            matches!(source, Some(FirstPartySource::Directory(_))),
            "{identity}: {source:?}"
        );
    }
    assert!(FIRST_PARTY_BUNDLES.contains(&"hya/core-commands"));
    assert!(first_party_source(Path::new("/nowhere/target/debug"), "hya/unknown").is_none());
    assert!(first_party_source(Path::new("/nowhere/target/debug"), "acme/core-skills").is_none());
}

#[test]
fn installed_layout_loads_package_beside_bin_and_cargo_layout_prefers_in_tree() {
    let scratch = Scratch::new("layouts");
    let package = stage_package(
        &scratch.0.join("bundles"),
        "hya/core-skills",
        "hya/core-skills",
    );
    let source = first_party_source(&scratch.0.join("bin"), "hya/core-skills");
    assert_eq!(source, Some(FirstPartySource::Package(package)));

    let catalog = load_first_party(&source.expect("package source"), "hya/core-skills")
        .expect("load installed package");
    let expected = prepare_package(
        BundleSource::read_directory(first_party_source_root().join("presets/core-skills"))
            .expect("read source"),
    )
    .expect("prepare source");
    assert_eq!(catalog.digest(), expected.digest());
    assert_eq!(catalog.bundles()[0].identity().id, "hya/core-skills");
    assert_eq!(
        first_party_source(&scratch.0.join("empty/bin"), "hya/core-skills"),
        None
    );

    let debug = scratch.0.join("target/debug");
    stage_package(&debug.join("bundles"), "hya/core-skills", "hya/core-skills");
    for directory in [debug.clone(), debug.join("deps")] {
        assert_eq!(
            first_party_source(&directory, "hya/core-skills"),
            Some(FirstPartySource::Directory(
                first_party_source_root().join("presets/core-skills")
            )),
            "{}",
            directory.display()
        );
    }
}

#[test]
fn package_with_another_identity_is_rejected() {
    let scratch = Scratch::new("mismatch");
    let package = stage_package(
        &scratch.0.join("bundles"),
        "hya/core-agents",
        "hya/core-skills",
    );
    let error = load_first_party(&FirstPartySource::Package(package), "hya/core-agents")
        .expect_err("identity mismatch must fail");
    assert!(
        matches!(error, BundleError::FirstPartyBundle { ref identity, .. } if identity == "hya/core-agents"),
        "{error}"
    );
}

#[test]
fn process_catalog_is_loaded_once_and_shared_concurrently() {
    let first = first_party_bundle("hya/core-skills").expect("load core skills");
    let second = first_party_bundle("hya/core-skills").expect("reuse core skills");
    assert!(std::ptr::eq(first, second));
    assert_eq!(first.bundles()[0].identity().id, "hya/core-skills");
    assert!(first_party_bundle("hya/unknown").is_err());

    std::thread::scope(|scope| {
        let workers = (0..24)
            .map(|index| {
                scope.spawn(move || {
                    let identity = FIRST_PARTY_BUNDLES[index % FIRST_PARTY_BUNDLES.len()];
                    (
                        identity,
                        first_party_bundle(identity).expect("load first-party catalog"),
                    )
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            let (identity, catalog) = worker.join().expect("catalog worker");
            assert!(std::ptr::eq(catalog, first_party_bundle(identity).unwrap()));
        }
    });
}

#[test]
fn no_crate_compiles_first_party_bundle_content_into_the_binary() {
    let workspace = first_party_source_root().join("..");
    let mut offenders = Vec::new();
    let mut stack = vec![workspace.join("crates")];
    stack.extend(
        std::fs::read_dir(first_party_source_root().join("presets"))
            .expect("list presets")
            .map(|entry| entry.expect("preset entry").path().join("native")),
    );
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name == "target" || name == "tests")
            {
                continue;
            }
            stack.extend(
                std::fs::read_dir(&path)
                    .expect("read source directory")
                    .map(|entry| entry.expect("source entry").path()),
            );
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read Rust source");
        let build_script = path.file_name().is_some_and(|name| name == "build.rs");
        let embeds = text.lines().any(|line| {
            (line.contains("include_str!(") || line.contains("include_bytes!("))
                && (line.contains("bundles/") || line.contains("OUT_DIR"))
        });
        if embeds || (build_script && text.contains("bundles/")) {
            offenders.push(
                path.strip_prefix(&workspace)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "first-party bundle content must load at runtime, not compile in: {offenders:?}"
    );
}

#[test]
fn first_party_identity_versions_follow_the_hya_version() {
    for identity in FIRST_PARTY_BUNDLES {
        let catalog = first_party_bundle(identity).expect("load first-party bundle");
        assert_eq!(
            catalog.bundles()[0].identity().version,
            hya_version::BACKEND_VERSION,
            "{identity} must be released at the backend version"
        );
    }
}

#[test]
fn orchestrator_runtime_is_a_copy_of_the_code_mode_runtime() {
    let bundles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundles");
    let read = |path: &str| std::fs::read(bundles.join(path)).expect("runtime source");
    assert!(
        read("first-party/code-mode/runtime.ts") == read("extra/orchestrator/runtime.ts"),
        "copy bundles/first-party/code-mode/runtime.ts to bundles/extra/orchestrator/runtime.ts"
    );
}

#[test]
fn every_extra_bundle_prepares_and_packages_deterministically() {
    let dirs = extra_bundle_dirs();
    assert!(
        !dirs.is_empty(),
        "expected at least one bundles/extra/* source directory"
    );
    for dir in &dirs {
        let source = BundleSource::read_directory(dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        let prepared = prepare_package(source.clone())
            .unwrap_or_else(|error| panic!("prepare {}: {error}", dir.display()));
        assert_eq!(prepared.bundles().len(), 1, "{}", dir.display());
        let identity = prepared.bundles()[0].identity();
        assert!(identity.id.starts_with("hya-extra/"), "{}", identity.id);
        assert_eq!(identity.publisher, "hya-extra", "{}", dir.display());
        assert_eq!(
            identity.version,
            hya_version::BACKEND_VERSION,
            "{}",
            dir.display()
        );

        let first = write_public_package(&source).expect("package extra bundle");
        assert_eq!(first, write_public_package(&source).expect("repackage"));
        let reprepared = inspect_public_package(&first).expect("inspect packaged extra");
        assert_eq!(reprepared.digest(), prepared.digest(), "{}", dir.display());
    }
}

#[test]
fn extra_bundles_expose_expected_plugins_agents_hooks_and_apis() {
    let load = |name: &str| {
        prepare_package(
            BundleSource::read_directory(first_party_source_root().join("extra").join(name))
                .expect("read extra bundle"),
        )
        .expect("prepare extra bundle")
    };

    // zvec-grep: agentless plugin with 1 MCP server and 1 skill.
    let zvec = load("zvec-grep");
    let zb = &zvec.bundles()[0];
    assert_eq!(zb.kind(), PreparedBundleKind::Bundle);
    assert!(zb.agents().is_empty());
    assert_eq!(zb.mcp().len(), 1);
    assert_eq!(zb.mcp()[0].local_id, "zvec-grep");
    assert_eq!(zb.skills().len(), 1);
    assert_eq!(zb.skills()[0].local_id, "zvec-grep");

    // scout: subagent with its own MCP server and domain-only resource_view.
    let scout = load("scout");
    let sb = &scout.bundles()[0];
    assert_eq!(sb.agents().len(), 1);
    assert_eq!(sb.agents()[0].id.as_str(), "scout");
    assert_eq!(sb.agents()[0].role, AgentRole::Subagent);
    assert_eq!(sb.mcp().len(), 1);
    let allow: Vec<&str> = sb.agents()[0]
        .resource_view
        .allow
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(allow.len(), 4);
    for expected in [
        "harness:tool/read",
        "harness:tool/grep",
        "harness:tool/glob",
        "bundle:hya-extra/scout/mcp/zvec-grep",
    ] {
        assert!(
            allow.contains(&expected),
            "scout missing {expected}: {allow:?}"
        );
    }
    assert!(!allow.contains(&"harness:tool/report"));

    // jev-model-router & model-fallback: bun process plugins with one hook each.
    for (dir, id, hook, script) in [
        (
            "jev-model-router",
            "hya-extra/jev-model-router",
            "chat.params",
            "router.ts",
        ),
        (
            "model-fallback",
            "hya-extra/model-fallback",
            "model.fallback",
            "fallback.ts",
        ),
    ] {
        let prepared = load(dir);
        let bundle = &prepared.bundles()[0];
        assert_eq!(bundle.namespace(), dir);
        assert!(bundle.agents().is_empty() && bundle.tools().is_empty());
        assert_eq!(
            bundle
                .hooks()
                .iter()
                .map(|h| h.local_id.as_str())
                .collect::<Vec<_>>(),
            [hook]
        );
        let process = prepared
            .bundle_process(id)
            .expect("explicit extensions.process");
        assert_eq!(process.kind, hya_bundle::PreparedProcessKind::Bun);
        assert_eq!(
            process.command,
            ["bun", "run", &format!("${{BUNDLE_ROOT}}/{script}")]
        );
        assert_eq!(
            bundle
                .extensions()
                .iter()
                .map(|f| f.source_path.as_str())
                .collect::<Vec<_>>(),
            [script]
        );
    }

    // token-summary: bun process plugin with one tool and one GET session /usage API.
    let ts = load("token-summary");
    let tb = &ts.bundles()[0];
    assert_eq!(tb.namespace(), "token-summary");
    assert!(tb.agents().is_empty() && tb.hooks().is_empty());
    assert_eq!(tb.tools().len(), 1);
    assert_eq!(tb.tools()[0].local_id, "token_summary");
    assert_eq!(tb.tools()[0].source_path, "summary.ts");
    let proc_ts = ts
        .bundle_process("hya-extra/token-summary")
        .expect("token-summary process");
    assert_eq!(proc_ts.kind, hya_bundle::PreparedProcessKind::Bun);
    assert_eq!(proc_ts.command, ["bun", "run", "${BUNDLE_ROOT}/summary.ts"]);
    let apis = ts.bundle_apis("hya-extra/token-summary");
    assert_eq!(apis.len(), 1);
    assert_eq!(apis[0].id, "usage");
    assert_eq!(apis[0].method, hya_bundle::ApiMethod::Get);
    assert_eq!(apis[0].scope, hya_bundle::ApiScope::Session);
    assert_eq!(apis[0].path, "/usage");
    assert_eq!(apis[0].request_schema, None);
    assert_eq!(
        apis[0].response_schema.as_deref(),
        Some("schemas/usage.json")
    );
    assert!(!apis[0].description.is_empty());
    assert_eq!(
        tb.extensions()
            .iter()
            .map(|f| f.source_path.as_str())
            .collect::<Vec<_>>(),
        ["summary.ts", "schemas/usage.json"]
    );
}

#[test]
fn docs_example_bundles_and_api_snippet_prepare_deterministically() {
    // 1. Single-file docs/examples/bundle.hya.md
    let bytes = std::fs::read(repository_root().join("docs/examples/bundle.hya.md"))
        .expect("docs example must exist");
    assert!(!bytes.is_empty());
    let first = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes.clone())],
    ))
    .expect("docs example prepares");
    let second = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes)],
    ))
    .expect("docs example second pass");
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    let agent = &first.bundles()[0].agents()[0];
    assert_eq!(agent.role, AgentRole::Main);
    assert!(
        agent
            .prompt
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
    );
    assert_eq!(agent.prompt_source.as_deref(), Some("bundle.hya.md"));

    // 2. bun-transient & bun-resident examples
    for (directory, bundle_id, stable_id) in [
        (
            "bun-transient",
            "hya/docs-bun-transient",
            "docs-bun-transient",
        ),
        ("bun-resident", "hya/docs-bun-resident", "docs-bun-resident"),
    ] {
        let (a, b) = prepare_bun_example(directory);
        assert_eq!(a.bytes(), b.bytes());
        assert_eq!(a.digest(), b.digest());
        let bundle = &a.bundles()[0];
        assert_eq!(bundle.identity().id, bundle_id);
        assert_eq!(bundle.tools().len(), 1);
        assert_eq!(bundle.extensions().len(), 1);
        assert!(bundle.skills().is_empty() && bundle.hooks().is_empty() && bundle.mcp().is_empty());
        assert_eq!(bundle.agents()[0].id.as_str(), stable_id);
        assert_eq!(bundle.agents()[0].role, AgentRole::Main);
        assert!(bundle.agents()[0].can_spawn.is_empty());
        assert_eq!(bundle.tools()[0].local_id, "echo");
        assert_eq!(
            bundle.tools()[0].stable_id,
            format!("bundle:{bundle_id}/tool/echo")
        );
        assert_eq!(bundle.extensions()[0].local_id, "runtime");
        assert!(bundle.extensions()[0].content.contains("export default"));
        assert_eq!(bundle.tools()[0].digest, bundle.extensions()[0].digest);
    }

    // 3. bun-disjoint example
    let (da, db) = prepare_bun_example("bun-disjoint");
    assert_eq!(da.bytes(), db.bytes());
    let bundle = &da.bundles()[0];
    assert_eq!(bundle.identity().id, "hya/docs-bun-disjoint");
    assert_eq!(bundle.tools().len(), 1);
    assert_eq!(bundle.hooks().len(), 1);
    assert_eq!(bundle.extensions().len(), 1);
    let alpha = &bundle.agents()[0];
    assert_eq!(alpha.id.as_str(), "docs-bun-alpha");
    assert_eq!(alpha.prompt_source.as_deref(), Some("bundle.hya.md"));
    assert_eq!(
        alpha.resource_view.allow,
        ["bundle:hya/docs-bun-disjoint/tool/echo"]
    );
    assert_eq!(alpha.hook_refs, ["bundle:hya/docs-bun-disjoint/hook/event"]);
    assert_eq!(bundle.tools()[0].digest, bundle.extensions()[0].digest);
    assert_eq!(bundle.hooks()[0].digest, bundle.extensions()[0].digest);

    // 4. Authoring doc `apis:` fenced YAML snippet
    let doc = authoring_doc();
    let manifest = fenced_block(&doc, "### API endpoints (`apis:`)", "yaml", 0);
    let catalog = prepare_package(BundleSource::new(
        "notes",
        vec![
            SourceFile::new("bundle.yaml", manifest),
            SourceFile::new("notes.ts", "// protocol process\n"),
            SourceFile::new("schemas/note.json", "{\"type\":\"object\"}"),
        ],
    ))
    .expect("the `apis:` example must prepare");
    assert_eq!(
        catalog
            .bundle_apis("acme/notes")
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["put-note", "usage"]
    );
}

#[test]
fn docs_capture_authoring_sidecar_and_cli_contracts() {
    let authoring_paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("bundles/presets/core-skills/resources/skills/agent-bundle-authoring/SKILL.md"),
    ];
    let authoring_markers = [
        "`hook_refs`",
        "supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`",
        "aliases do not rename hooks",
        "owning bundle",
        "exact-path",
        "only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list",
        "staged does not mean activated",
        "declarations of the loaded modules, regardless of order",
        "`resource_view` and hooks outside `hook_refs` are neither advertised nor dispatched",
        "the complete packaged Skill set, independently of the agent's Skill selection",
        "`agents`",
        "derived from its origin, not declared",
        "The clamp is not a sandbox.",
        "bun-disjoint",
        "two tools while two agents each select a different one",
        "The activation-scoped JS profile admits only self-contained selected Extension entrypoints; it does not load inert support files or discover transitive JS imports.",
        "external single-file bundling",
        "activation never executes the authoring tree",
        "undeclared directory files are ignored",
        "unreferenced archive files are rejected",
        "missing relative helper import fails before ACK",
        "`hook_refs` select Bundle-local Hook resources only",
        "all `harness:hook/*` spellings reject",
        "Harness host hooks stay outside Bundle metadata",
        "bundle.hya.md extensions/runtime.js",
    ];
    for path in &authoring_paths {
        let source = std::fs::read_to_string(path).expect("read authoring doc");
        assert!(
            !source.contains("bundle.hya.md tools extensions"),
            "{}",
            path.display()
        );
        assert!(
            !source.contains("validated transitive referenced closure"),
            "{}",
            path.display()
        );
        for marker in authoring_markers {
            assert!(
                source.contains(marker),
                "{} missing `{marker}`",
                path.display()
            );
        }
    }

    // Sidecar protocol version distinction across authoring + runtime docs.
    for path in [
        authoring_paths[0].clone(),
        authoring_paths[1].clone(),
        repository_root().join("docs/architecture/runtime.md"),
    ] {
        let normalized = std::fs::read_to_string(&path)
            .expect("read sidecar doc")
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for marker in [
            "newline-delimited JSON-RPC 2.0",
            "plugin protocol version 1",
            "initialize retains existing protocol_version and host fields",
            "the only activation-specific metadata is { activation_id, lifecycle }",
        ] {
            assert!(
                normalized.contains(marker),
                "{} missing `{marker}`",
                path.display()
            );
        }
        for stale in [
            "JSON-RPC v1",
            "Initialization is request/reply and carries exactly activation_id and lifecycle",
            "Initialization is request/reply and carries only activation_id and lifecycle",
            "Initialize is a request/reply ACK carrying only activation_id and lifecycle",
        ] {
            assert!(
                !normalized.contains(stale),
                "{} retained `{stale}`",
                path.display()
            );
        }
    }

    // CLI publication vs activation distinction.
    let cli = std::fs::read_to_string(repository_root().join("docs/cli.md"))
        .expect("read docs/cli.md")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for marker in [
        "publication validates the merged catalog after first-party shadowing, the complete installed BundleCatalog, and reserved core Agent ids before atomic generation publication",
        "activation materializes only the selected Agent's captured Tool/Hook/Skill capability closure and exact-path-matched JavaScript Extension entrypoints",
    ] {
        assert!(cli.contains(marker), "docs/cli.md missing `{marker}`");
    }
    assert!(!cli.contains("Publication and activation compile only the selected agent"));
}

#[test]
fn workflow_and_provider_docs_cover_control_replay_and_model_routing() {
    for path in workflow_doc_paths() {
        let source = std::fs::read_to_string(&path).expect("read workflow doc");
        assert!(source.lines().filter(|l| !l.trim().is_empty()).count() >= 30);
        assert!(!source.contains("Zero preset workflows"));
        let normalized = source.split_whitespace().collect::<Vec<_>>().join(" ");
        for marker in [
            "Bundle",
            "session.updated",
            "interrupted",
            "unavailable",
            "stale",
            "client",
            "sidebar",
            "WorkflowControl",
            "model:",
            "reasoning:",
            "fallback:",
            "#variant",
            "WorkflowStageRouteOutcome",
            "one route outcome per explicit-route provider stream group",
            "do not render model routes",
        ] {
            assert!(
                normalized.contains(marker),
                "{} missing `{marker}`",
                path.display()
            );
        }
    }

    let providers =
        std::fs::read_to_string(repository_root().join("docs/architecture/providers.md"))
            .expect("read providers.md");
    for marker in [
        "reasoning_default",
        "supports_reasoning_effort",
        "with_model_reasoning_defaults",
        "first match",
        "#variant",
        "Workflow",
    ] {
        assert!(
            providers.contains(marker),
            "providers.md missing `{marker}`"
        );
    }
}

#[test]
fn authoring_permission_mode_example_prepares_and_answers_the_hook() {
    use std::io::{BufRead, BufReader, Write};

    let doc = authoring_doc();
    let heading = "### Permission modes (`permission_modes:`)";
    let manifest = fenced_block(&doc, heading, "yaml", 0);
    let script = fenced_block(&doc, heading, "ts", 0);
    let catalog = prepare_package(BundleSource::new(
        "approver",
        vec![
            SourceFile::new("bundle.yaml", manifest),
            SourceFile::new("approver.ts", script.clone()),
            SourceFile::new("hooks/permission-approve.json", "{}"),
        ],
    ))
    .expect("the `permission_modes:` example must prepare");
    let modes = catalog.bundle_permission_modes("acme/approver");
    assert_eq!(modes.len(), 1);
    assert_eq!(modes[0].id, "careful");
    let namespace = catalog.bundles()[0].namespace().to_string();

    if !std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return;
    }
    let scratch = Scratch::new("docs-approver");
    std::fs::write(scratch.0.join("approver.ts"), &script).expect("write approver.ts");
    let mut child = std::process::Command::new("bun")
        .arg("run")
        .arg(scratch.0.join("approver.ts"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bun");
    let ask = |id: u64, mode: &str, action: &str, command: &str| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "hook/permission.approve",
            "params": {
                "session": "hysec_a", "root_session": "hysec_a", "agent": "build",
                "mode": mode, "action": action,
                "resource": {"type": "command", "value": command}
            }
        })
    };
    let requests = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocol_version": 1, "host": {"name": "hya", "version": "test"}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {}}),
        ask(2, "careful", "bash", "git status"),
        ask(3, "careful", "bash", "rm -rf build"),
        ask(4, "careful", "edit", "src/main.rs"),
        ask(5, "other", "bash", "ls"),
        serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "shutdown", "params": {}}),
    ];
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for request in &requests {
            writeln!(stdin, "{request}").expect("write request");
        }
    }
    let replies: Vec<serde_json::Value> = BufReader::new(child.stdout.take().expect("stdout"))
        .lines()
        .map(|line| serde_json::from_str(&line.expect("read line")).expect("JSON reply"))
        .collect();
    let _ = child.wait();

    assert_eq!(replies.len(), 6, "replies: {replies:?}");
    let init = &replies[0]["result"];
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(init["protocol_version"], 1);
    assert_eq!(init["plugin"]["id"], serde_json::json!(namespace));
    assert_eq!(init["plugin"]["kind"], "bun");
    assert_eq!(
        init["hooks"],
        serde_json::json!([{"name": "permission.approve"}])
    );
    let outcomes: Vec<(u64, &str)> = replies[1..5]
        .iter()
        .map(|r| {
            (
                r["id"].as_u64().unwrap_or_default(),
                r["result"]["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [(2, "allow_once"), (3, "defer"), (4, "defer"), (5, "defer")]
    );
    assert_eq!(
        replies[5],
        serde_json::json!({"jsonrpc": "2.0", "id": 6, "result": {}})
    );
}
