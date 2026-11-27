use std::collections::BTreeSet;

use super::parse::parse_source;
use crate::error::BundleError;
use crate::source::BundleSource;

#[test]
fn tool_family_manifests_declare_every_native_source_file() -> Result<(), BundleError> {
    let builtin = crate::first_party_source_root().join("builtin");
    let mut missing = Vec::new();
    for family in [
        "base-tools",
        "extended-tools",
        "network-tools",
        "channel-tools",
    ] {
        let source = parse_source(BundleSource::read_directory(builtin.join(family))?)?;
        let declared = source
            .manifest
            .extensions
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        missing.extend(
            source
                .files
                .keys()
                .filter(|path| path.starts_with("native/") && !declared.contains(path.as_str()))
                .map(|path| format!("{family}/{path}")),
        );
    }
    assert!(
        missing.is_empty(),
        "undeclared native source files: {missing:?}"
    );
    Ok(())
}
