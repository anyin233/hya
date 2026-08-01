//! Prove the repository documentation example is prepare-valid through the
//! production v1 Markdown preparer.

use std::path::{Path, PathBuf};

use hya_bundle::{AgentRole, BundleSource, SourceFile, SpawnLifecycle, prepare_builtins};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("hya-bundle must live under <repository>/crates"))
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
    let first = prepare_builtins(vec![BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes.clone())],
    )]);
    let first = first.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully: {error:?}");
    });
    let second = prepare_builtins(vec![BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes)],
    )]);
    let second = second.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully on second pass: {error:?}");
    });

    // Then: preparation succeeds with one flat main/transient agent and is deterministic.
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.bundles().len(), 1);
    assert_eq!(first.index().len(), 1);

    let bundle = &first.bundles()[0];
    assert_eq!(bundle.agents.len(), 1);
    let agent = &bundle.agents[0];
    assert_eq!(agent.role, AgentRole::Main);
    assert_eq!(agent.spawn_lifecycle, SpawnLifecycle::Transient);
    assert!(
        agent
            .prompt
            .as_deref()
            .is_some_and(|prompt| !prompt.trim().is_empty()),
        "markdown body must become a nonempty prepared prompt"
    );
    assert_eq!(agent.prompt_source.as_deref(), Some("bundle.hya.md"));
}
