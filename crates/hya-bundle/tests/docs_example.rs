//! Prove the repository documentation examples are prepare-valid through the
//! production preparers (and that the authoring guide's protocol script
//! actually speaks plugin protocol v1).

use std::path::{Path, PathBuf};

use hya_bundle::{AgentRole, BundleSource, PreparedCatalog, SourceFile, prepare_package};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("hya-bundle must live under <repository>/crates"))
}

/// Committed Workflow docs, plus the generated architecture wiki page when this
/// checkout has one. The wiki lives in the git-ignored `.autors/`, so a clean
/// clone (CI) checks the committed docs only.
fn workflow_doc_paths() -> Vec<PathBuf> {
    let root = repository_root();
    let mut paths = vec![root.join("docs/workflows.md")];
    if root.join(".autors/hya/wiki").is_dir() {
        paths.push(root.join(".autors/hya/wiki/pages/architecture/workflow-composition.md"));
    }
    paths
}

fn docs_example_path() -> PathBuf {
    repository_root().join("docs/examples/bundle.hya.md")
}

#[test]
fn docs_example_bundle_hya_md_prepares_deterministically() {
    // Given: the repository documentation example named exactly bundle.hya.md.
    let path = docs_example_path();
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("docs example must exist at {}: {error}", path.display()));
    assert!(
        !bytes.is_empty(),
        "docs example at {} must be nonempty preparer source",
        path.display()
    );

    // When: the example is passed as a single SourceFile through the production preparer.
    let first = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes.clone())],
    ));
    let first = first.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully: {error:?}");
    });
    let second = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes)],
    ));
    let second = second.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully on second pass: {error:?}");
    });

    // Then: preparation succeeds with one flat main agent and is deterministic.
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.bundles().len(), 1);
    assert_eq!(first.index().len(), 1);

    let bundle = &first.bundles()[0];
    let agent = &bundle.agents()[0];
    assert_eq!(agent.role, AgentRole::Main);
    assert!(
        agent
            .prompt
            .as_deref()
            .is_some_and(|prompt| !prompt.trim().is_empty()),
        "markdown body must become a nonempty prepared prompt"
    );
    assert_eq!(agent.prompt_source.as_deref(), Some("bundle.hya.md"));
}

#[test]
fn bundle_authoring_commands_enumerate_regular_closure_files() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("bundles/presets/core-skills/resources/skills/agent-bundle-authoring/SKILL.md"),
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "bundle authoring documentation must exist at {}: {error}",
                path.display()
            )
        });
        assert!(
            !source.contains("bundle.hya.md tools extensions"),
            "{} must not pass directories as archive inputs",
            path.display()
        );
        assert!(
            source.contains("bundle.hya.md extensions/runtime.js"),
            "{} must enumerate regular closure files as archive inputs",
            path.display()
        );
    }
}

#[test]
fn bundle_authoring_docs_capture_hook_and_entrypoint_contract() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("bundles/presets/core-skills/resources/skills/agent-bundle-authoring/SKILL.md"),
    ];
    let required_markers = [
        ("hook_refs", "`hook_refs`"),
        (
            "supported hook IDs",
            "supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`",
        ),
        ("hook aliases", "aliases do not rename hooks"),
        ("owning bundle", "owning bundle"),
        ("exact path", "exact-path"),
        (
            "selected entrypoints",
            "only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list",
        ),
        ("staging", "staged does not mean activated"),
        (
            "declaration validation",
            "Tool and Hook initialize declarations independently equal the selected expected sets regardless of order; missing, extra, duplicate, or unselected declarations reject",
        ),
        ("tool-only", "tool-only reports zero hooks"),
        ("hook-only", "hook-only reports zero tools"),
        (
            "one agent per AgentBundle",
            "An AgentBundle defines exactly one agent",
        ),
        (
            "host-controlled tool plane",
            "derived from its origin, not declared",
        ),
        ("clamp is not a sandbox", "The clamp is not a sandbox."),
        ("bun-disjoint link/name", "bun-disjoint"),
        (
            "generic superset modules",
            "generic superset modules are rejected and must be split",
        ),
        (
            "self-contained public JS profile",
            "The activation-scoped JS profile admits only self-contained selected Extension entrypoints; it does not load inert support files or discover transitive JS imports.",
        ),
        (
            "external single-file bundling",
            "external single-file bundling",
        ),
        (
            "authoring-tree isolation",
            "activation never executes the authoring tree",
        ),
        (
            "undeclared directory files",
            "undeclared directory files are ignored",
        ),
        (
            "unreferenced archive files",
            "unreferenced archive files are rejected",
        ),
        (
            "missing relative helper import",
            "missing relative helper import fails before ACK",
        ),
        (
            "Bundle-local hook refs",
            "`hook_refs` select Bundle-local Hook resources only",
        ),
        (
            "harness hook rejection",
            "all `harness:hook/*` spellings reject",
        ),
        (
            "Harness host hooks",
            "Harness host hooks stay outside AgentBundle metadata",
        ),
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "bundle authoring documentation must exist at {}: {error}",
                path.display()
            )
        });
        for (label, marker) in required_markers {
            assert!(
                source.contains(marker),
                "{} must contain docs contract marker `{label}`: {marker}",
                path.display()
            );
        }
        assert!(
            !source.contains("validated transitive referenced closure"),
            "{} must not retain stale transitive-closure wording",
            path.display()
        );
    }
}

#[test]
fn bundle_sidecar_docs_distinguish_jsonrpc_and_plugin_protocol_versions() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("bundles/presets/core-skills/resources/skills/agent-bundle-authoring/SKILL.md"),
        repository_root().join("docs/architecture/runtime.md"),
    ];
    let required_markers = [
        "newline-delimited JSON-RPC 2.0",
        "plugin protocol version 1",
        "initialize retains existing protocol_version and host fields",
        "the only activation-specific metadata is { activation_id, lifecycle }",
    ];
    let stale_markers = [
        "JSON-RPC v1",
        "Initialization is request/reply and carries exactly activation_id and lifecycle",
        "Initialization is request/reply and carries only activation_id and lifecycle",
        "Initialize is a request/reply ACK carrying only activation_id and lifecycle",
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "sidecar protocol documentation must exist at {}: {error}",
                path.display()
            )
        });
        let normalized = source
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for marker in required_markers {
            assert!(
                normalized.contains(marker),
                "{} must contain sidecar protocol marker `{marker}`",
                path.display()
            );
        }
        for marker in stale_markers {
            assert!(
                !normalized.contains(marker),
                "{} must not retain stale sidecar protocol wording `{marker}`",
                path.display()
            );
        }
    }
}

#[test]
fn bundle_cli_docs_distinguish_catalog_publication_from_activation_closure() {
    let source = std::fs::read_to_string(repository_root().join("docs/cli.md"))
        .unwrap_or_else(|error| panic!("CLI documentation must exist: {error}"));
    let normalized = source
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    for marker in [
        "publication validates the merged catalog after first-party shadowing, the complete installed BundleCatalog, and reserved core Agent ids before atomic generation publication",
        "activation materializes only the selected Agent's captured Tool/Hook/Skill capability closure and exact-path-matched JavaScript Extension entrypoints",
    ] {
        assert!(
            normalized.contains(marker),
            "docs/cli.md must contain CLI contract marker `{marker}`"
        );
    }
    assert!(
        !normalized.contains("Publication and activation compile only the selected agent"),
        "docs/cli.md must not combine publication and activation into one selected closure"
    );
}

/// Ensure the user guide and generated architecture wiki explain the durable
/// Workflow product surface instead of only the compiler internals.
#[test]
fn workflow_docs_cover_control_replay_and_client_state() {
    let paths = workflow_doc_paths();
    let required_markers = [
        "WorkflowBundle",
        "session.updated",
        "interrupted",
        "unavailable",
        "stale",
        "client",
        "sidebar",
        "WorkflowControl",
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "Workflow documentation must exist at {}: {error}",
                path.display()
            )
        });
        assert!(
            source
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                >= 30,
            "{} must contain a substantive Workflow explanation",
            path.display()
        );
        for marker in required_markers {
            assert!(
                source.contains(marker),
                "{} must contain Workflow contract marker `{marker}`",
                path.display()
            );
        }
        assert!(
            !source.contains("Zero preset workflows"),
            "{} must not claim that selectable first-party Workflows are absent",
            path.display()
        );
    }
}

/// Ensure Workflow and provider docs describe the suffix-free model-routing contract.
#[test]
fn workflow_docs_cover_stage_model_routing_and_route_outcomes() {
    for path in workflow_doc_paths() {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "Workflow routing documentation must exist at {}: {error}",
                path.display()
            )
        });
        let normalized = source.split_whitespace().collect::<Vec<_>>().join(" ");
        for marker in [
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
                "{} must contain Workflow routing marker `{marker}`",
                path.display()
            );
        }
    }

    let providers = repository_root().join("docs/architecture/providers.md");
    let source = std::fs::read_to_string(&providers).unwrap_or_else(|error| {
        panic!(
            "provider routing documentation must exist at {}: {error}",
            providers.display()
        )
    });
    for marker in [
        "reasoning_default",
        "supports_reasoning_effort",
        "with_model_reasoning_defaults",
        "first match",
        "#variant",
        "Workflow",
    ] {
        assert!(
            source.contains(marker),
            "{} must contain provider routing marker `{marker}`",
            providers.display()
        );
    }
}

struct ExpectedAgent {
    stable_id: &'static str,
    role: AgentRole,
    can_spawn: &'static [&'static str],
}

struct ExpectedExample {
    directory: &'static str,
    bundle_id: &'static str,
    agents: &'static [ExpectedAgent],
}

const NO_SPAWN: &[&str] = &[];
const TRANSIENT_AGENTS: &[ExpectedAgent] = &[ExpectedAgent {
    stable_id: "docs-bun-transient",
    role: AgentRole::Main,
    can_spawn: NO_SPAWN,
}];
const RESIDENT_AGENTS: &[ExpectedAgent] = &[ExpectedAgent {
    stable_id: "docs-bun-resident",
    role: AgentRole::Main,
    can_spawn: NO_SPAWN,
}];
const BUN_EXAMPLES: &[ExpectedExample] = &[
    ExpectedExample {
        directory: "bun-transient",
        bundle_id: "hya/docs-bun-transient",
        agents: TRANSIENT_AGENTS,
    },
    ExpectedExample {
        directory: "bun-resident",
        bundle_id: "hya/docs-bun-resident",
        agents: RESIDENT_AGENTS,
    },
];

fn prepare_bun_example(directory: &str) -> (PreparedCatalog, PreparedCatalog) {
    let path = repository_root().join("docs/examples").join(directory);
    let source = BundleSource::read_directory(&path).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must exist: {error}",
            path.display()
        )
    });
    let first = prepare_package(source.clone()).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must prepare: {error}",
            path.display()
        );
    });
    let second = prepare_package(source).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must prepare deterministically: {error}",
            path.display()
        );
    });
    (first, second)
}

#[test]
fn bun_examples_are_prepare_valid_and_deterministic() {
    for expected in BUN_EXAMPLES {
        let (first, second) = prepare_bun_example(expected.directory);
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.bundles().len(), 1);
        let bundle = &first.bundles()[0];
        assert_eq!(bundle.identity().id, expected.bundle_id);
        assert_eq!(bundle.tools().len(), 1);
        assert_eq!(bundle.extensions().len(), 1);
        assert!(bundle.skills().is_empty());
        assert!(bundle.hooks().is_empty());
        assert!(bundle.mcp().is_empty());

        {
            let agent = &bundle.agents()[0];
            let [expected_agent] = expected.agents else {
                panic!("a bundle example declares exactly one agent");
            };
            assert_eq!(agent.id.as_str(), expected_agent.stable_id);
            assert_eq!(agent.role, expected_agent.role);
            let can_spawn = agent
                .can_spawn
                .iter()
                .map(|agent| agent.as_str())
                .collect::<Vec<_>>();
            assert_eq!(can_spawn.as_slice(), expected_agent.can_spawn);
        }

        let tool = &bundle.tools()[0];
        assert_eq!(tool.local_id, "echo");
        assert_eq!(tool.source_path, "extensions/runtime.js");
        assert_eq!(
            tool.stable_id,
            format!("bundle:{}/tool/echo", expected.bundle_id)
        );
        assert!(!tool.content.trim().is_empty());
        assert!(!tool.digest.trim().is_empty());

        let extension = &bundle.extensions()[0];
        assert_eq!(extension.local_id, "runtime");
        assert_eq!(extension.source_path, "extensions/runtime.js");
        assert_eq!(
            extension.stable_id,
            format!("bundle:{}/extension/runtime", expected.bundle_id)
        );
        assert!(extension.content.contains("export default"));
        assert!(!extension.digest.trim().is_empty());
        assert_eq!(tool.content, extension.content);
        assert_eq!(tool.digest, extension.digest);
    }
}

#[test]
fn bun_disjoint_example_is_prepare_valid_and_captures_the_agent_closure() {
    let (first, second) = prepare_bun_example("bun-disjoint");
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.bundles().len(), 1);

    let bundle = &first.bundles()[0];
    assert_eq!(bundle.identity().id, "hya/docs-bun-disjoint");
    assert_eq!(bundle.tools().len(), 1);
    assert_eq!(bundle.hooks().len(), 1);
    assert_eq!(bundle.extensions().len(), 1);
    assert!(bundle.skills().is_empty());
    assert!(bundle.mcp().is_empty());

    fn find_resource<'a>(
        resources: &'a [hya_bundle::PreparedResource],
        local_id: &str,
    ) -> &'a hya_bundle::PreparedResource {
        resources
            .iter()
            .find(|resource| resource.local_id == local_id)
            .unwrap_or_else(|| panic!("prepared resource `{local_id}` is missing"))
    }
    let assert_matches_extension = |resource: &hya_bundle::PreparedResource| {
        let extension = &bundle.extensions()[0];
        assert_eq!(resource.source_path, extension.source_path);
        assert_eq!(resource.content, extension.content);
        assert_eq!(resource.digest, extension.digest);
    };

    let alpha = &bundle.agents()[0];
    assert_eq!(alpha.id.as_str(), "docs-bun-alpha");
    assert_eq!(alpha.role, AgentRole::Main);
    // The Markdown body is the prompt; the agent names no prompt resource.
    assert_eq!(alpha.prompt_source.as_deref(), Some("bundle.hya.md"));
    assert_eq!(
        alpha.resource_view.allow,
        ["bundle:hya/docs-bun-disjoint/tool/echo"]
    );
    assert_eq!(alpha.hook_refs, ["bundle:hya/docs-bun-disjoint/hook/event"]);

    let echo = find_resource(bundle.tools(), "echo");
    assert_eq!(echo.stable_id, "bundle:hya/docs-bun-disjoint/tool/echo");
    assert_matches_extension(echo);

    let event = find_resource(bundle.hooks(), "event");
    assert_eq!(event.stable_id, "bundle:hya/docs-bun-disjoint/hook/event");
    assert_matches_extension(event);
}

/// The `index`-th fenced block of `language` after `heading` in `doc`.
fn fenced_block(doc: &str, heading: &str, language: &str, index: usize) -> String {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("heading `{heading}` must exist"));
    let fence = format!("```{language}\n");
    let mut rest = &doc[start..];
    for _ in 0..index {
        let skip = rest.find(&fence).unwrap_or_else(|| panic!("missing block"));
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
        .unwrap_or_else(|error| panic!("docs/agent-bundle-authoring.md must exist: {error}"))
}

#[test]
fn authoring_api_endpoint_example_prepares() {
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
    .unwrap_or_else(|error| panic!("the `apis:` example must prepare: {error:?}"));
    let ids: Vec<&str> = catalog
        .bundle_apis("acme/notes")
        .iter()
        .map(|api| api.id.as_str())
        .collect();
    assert_eq!(ids, ["put-note", "usage"]);
}

/// The `permission_modes:` example prepares, and its `approver.ts` really
/// speaks plugin protocol v1: it answers `initialize` with the bundle's
/// namespace and the `permission.approve` hook, then decides asks. The
/// process half runs only where `bun` is installed.
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
    .unwrap_or_else(|error| panic!("the `permission_modes:` example must prepare: {error:?}"));
    let modes = catalog.bundle_permission_modes("acme/approver");
    assert_eq!(modes.len(), 1);
    assert_eq!(modes[0].id, "careful");
    let namespace = catalog.bundles()[0].namespace().to_string();

    if !std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("bun not installed: skipping the approver.ts protocol round trip");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "hya-docs-approver-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("temp dir: {error}"));
    std::fs::write(dir.join("approver.ts"), &script)
        .unwrap_or_else(|error| panic!("write approver.ts: {error}"));
    let mut child = std::process::Command::new("bun")
        .arg("run")
        .arg(dir.join("approver.ts"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn bun: {error}"));
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
        let mut stdin = child.stdin.take().unwrap_or_else(|| panic!("stdin"));
        for request in &requests {
            writeln!(stdin, "{request}").unwrap_or_else(|error| panic!("write: {error}"));
        }
    }
    let stdout = child.stdout.take().unwrap_or_else(|| panic!("stdout"));
    let replies: Vec<serde_json::Value> = BufReader::new(stdout)
        .lines()
        .map(|line| {
            serde_json::from_str(&line.unwrap_or_else(|error| panic!("read: {error}")))
                .unwrap_or_else(|error| panic!("reply must be JSON: {error}"))
        })
        .collect();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        replies.len(),
        6,
        "one reply per request, none for the notification: {replies:?}"
    );
    let init = &replies[0]["result"];
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(init["protocol_version"], 1);
    assert_eq!(init["plugin"]["id"], serde_json::json!(namespace));
    assert_eq!(init["plugin"]["kind"], "bun");
    assert_eq!(
        init["hooks"],
        serde_json::json!([{"name": "permission.approve"}])
    );
    let outcomes: Vec<(u64, String)> = replies[1..5]
        .iter()
        .map(|reply| {
            (
                reply["id"].as_u64().unwrap_or_default(),
                reply["result"]["outcome"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (2, "allow_once".to_string()),
            (3, "defer".to_string()),
            (4, "defer".to_string()),
            (5, "defer".to_string()),
        ]
    );
    assert_eq!(
        replies[5],
        serde_json::json!({"jsonrpc": "2.0", "id": 6, "result": {}})
    );
}
