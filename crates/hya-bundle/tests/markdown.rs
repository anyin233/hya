//! Single-file markdown bundles: manifest markers and exact-filename rules.

use hya_bundle::{BundleError, BundleSource, SourceFile, prepare_package};

fn markdown(
    frontmatter: &str,
    file_name: &str,
) -> Result<hya_bundle::PreparedCatalog, BundleError> {
    prepare_package(BundleSource::new(
        "markdown",
        vec![SourceFile::new(
            file_name,
            format!("---\n{frontmatter}\n---\nYou are the Markdown lead.\n").into_bytes(),
        )],
    ))
}

fn manifest_markers() -> &'static str {
    r#"kind: AgentBundle
identity:
  id: hya/markdown
  version: 1.0.0
  publisher: hya
agent:
  id: markdown-lead
  role: main
  spawn_lifecycle: transient"#
}

#[test]
fn single_markdown_requires_the_exact_filename_and_the_kind_marker() {
    let valid = markdown(manifest_markers(), "bundle.hya.md");
    let Ok(valid) = valid else {
        panic!("valid single Markdown failed: {valid:?}");
    };
    assert_eq!(
        valid.bundles()[0].agent.prompt.as_deref(),
        Some("You are the Markdown lead.")
    );

    let ordinary = markdown(manifest_markers(), "agent.md");
    assert!(matches!(
        ordinary,
        Err(BundleError::UnsupportedSource { .. })
    ));

    // `api_version` was removed with the single-agent format; a manifest that
    // still carries it is rejected by name, not by a generic parse error.
    let stale_api_version = markdown(
        &format!("api_version: hya.agent-bundle/v1\n{}", manifest_markers()),
        "bundle.hya.md",
    );
    assert!(matches!(
        stale_api_version,
        Err(BundleError::RemovedManifestKey { ref key, .. }) if key == "api_version"
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
fn empty_body_markdown_is_allowed_when_the_agent_names_a_prompt_file() {
    // An empty Markdown body next to an explicit `prompt:` is "no body", not an
    // empty prompt, so the named file still wins.
    let prepared = prepare_package(BundleSource::new(
        "markdown-explicit-prompt",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: AgentBundle
identity:
  id: hya/markdown-explicit-prompt
  version: 1.0.0
  publisher: hya
agent:
  id: markdown-alpha
  role: main
  prompt: prompts/alpha.md
  spawn_lifecycle: transient
---

"#,
            ),
            SourceFile::new("prompts/alpha.md", b"Alpha prompt."),
        ],
    ));
    let Ok(prepared) = prepared else {
        panic!("explicit-prompt Markdown source must prepare successfully: {prepared:?}");
    };

    assert_eq!(prepared.bundles().len(), 1);
    let agent = &prepared.bundles()[0].agent;
    assert_eq!(agent.id.as_str(), "markdown-alpha");
    assert_eq!(agent.prompt.as_deref(), Some("Alpha prompt."));
    assert_eq!(agent.prompt_source.as_deref(), Some("prompts/alpha.md"));
}
