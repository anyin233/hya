//! Prerequisite check over a composed set of enabled bundles.

use std::collections::BTreeSet;

use crate::Bundle;

/// `(bundle_id, missing)` for every `requires` entry of `bundles` that the set
/// does not satisfy, in input order.
///
/// A requirement is satisfied by an id in `present` (bundles enabled outside
/// `bundles`), by a bundle in `bundles` with that id, or by one that
/// `overrides` it (an overrider replaces its target).
#[must_use]
pub fn missing_prerequisites<'a>(
    bundles: &[&'a Bundle],
    present: &BTreeSet<&str>,
) -> Vec<(&'a str, &'a str)> {
    let provided = bundles
        .iter()
        .flat_map(|bundle| [Some(bundle.identity().id.as_str()), bundle.overrides()])
        .flatten()
        .collect::<BTreeSet<_>>();
    bundles
        .iter()
        .flat_map(|bundle| {
            bundle
                .requires()
                .iter()
                .filter(|required| {
                    !present.contains(required.as_str()) && !provided.contains(required.as_str())
                })
                .map(|required| (bundle.identity().id.as_str(), required.as_str()))
        })
        .collect()
}
