use hya_bundle::{BundleError, BundleSource, SourceFile, prepare_builtins};

fn markdown(
    frontmatter: &str,
    file_name: &str,
) -> Result<hya_bundle::PreparedCatalog, BundleError> {
    prepare_builtins(vec![BundleSource::new(
        "markdown",
        vec![SourceFile::new(
            file_name,
            format!("---\n{frontmatter}\n---\nYou are the Markdown lead.\n").into_bytes(),
        )],
    )])
}

fn manifest_markers() -> &'static str {
    r#"api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/markdown
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: markdown-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full"#
}

#[test]
fn single_markdown_requires_exact_filename_and_both_v1_markers() {
    let valid = markdown(manifest_markers(), "bundle.hya.md");
    let Ok(valid) = valid else {
        panic!("valid single Markdown failed: {valid:?}");
    };
    assert_eq!(
        valid.bundles()[0].agents[0].prompt.as_deref(),
        Some("You are the Markdown lead.")
    );

    let ordinary = markdown(manifest_markers(), "agent.md");
    assert!(matches!(
        ordinary,
        Err(BundleError::UnsupportedSource { .. })
    ));

    let missing_api = markdown(
        &manifest_markers().replace("api_version: hya.agent-bundle/v1\n", ""),
        "bundle.hya.md",
    );
    assert!(matches!(
        missing_api,
        Err(BundleError::InvalidManifest { .. })
    ));

    let missing_kind = markdown(
        &manifest_markers().replace("kind: AgentBundle\n", ""),
        "bundle.hya.md",
    );
    assert!(matches!(
        missing_kind,
        Err(BundleError::InvalidManifest { .. })
    ));
}

#[test]
fn empty_body_markdown_supports_multiple_agents_with_explicit_prompt_files() {
    let prepared = prepare_builtins(vec![BundleSource::new(
        "markdown-multi-agent",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                br#"---
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/markdown-multi-agent
  version: 1.0.0
  publisher: hya
agents:
  - local_id: alpha
    stable_id: markdown-alpha
    role: main
    prompt: prompts/alpha.md
    spawn_lifecycle: transient
    harness_access: full
  - local_id: beta
    stable_id: markdown-beta
    role: subagent
    prompt: prompts/beta.md
    spawn_lifecycle: transient
    harness_access: full
---

"#,
            ),
            SourceFile::new("prompts/alpha.md", b"Alpha prompt."),
            SourceFile::new("prompts/beta.md", b"Beta prompt."),
        ],
    )]);
    let Ok(prepared) = prepared else {
        panic!("multi-agent Markdown source must prepare successfully: {prepared:?}");
    };

    assert_eq!(prepared.bundles().len(), 1);
    let agents = &prepared.bundles()[0].agents;
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].stable_id.as_str(), "markdown-alpha");
    assert_eq!(agents[0].prompt.as_deref(), Some("Alpha prompt."));
    assert_eq!(agents[0].prompt_source.as_deref(), Some("prompts/alpha.md"));
    assert_eq!(agents[1].stable_id.as_str(), "markdown-beta");
    assert_eq!(agents[1].prompt.as_deref(), Some("Beta prompt."));
    assert_eq!(agents[1].prompt_source.as_deref(), Some("prompts/beta.md"));
}
