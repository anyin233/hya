//! Canonical v1-to-v2 built-in bundle ID remapping and config-dir leaf encoding.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Outcome of remapping one v1 bundle ID to its v2 target(s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleIdRemap {
    /// Legacy ID expanded into one or more canonical v2 built-in bundle IDs.
    Remapped {
        /// Original v1 bundle identifier.
        from: String,
        /// Target v2 canonical bundle identifiers.
        to: Vec<&'static str>,
    },
    /// Legacy bundle was promoted into `hya-bare` core and no longer exists as a bundle.
    PromotedToCore {
        /// Original v1 bundle identifier.
        from: String,
        /// Explanation for diagnostics.
        note: &'static str,
    },
    /// Identifier is already canonical v2 or a user-defined bundle ID.
    Unchanged(String),
}

/// Remap a v1 bundle identifier (bare or `hya/`-prefixed) to its v2 equivalent(s)
/// per ADR-0039.
#[must_use]
pub fn remap_bundle_id(raw: &str) -> BundleIdRemap {
    let trimmed = raw.trim();
    match trimmed {
        "base-tools" | "hya/base-tools" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/hashline", "hya/coding-extras"],
        },
        "extended-tools" | "hya/extended-tools" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec![
                "hya/coding-extras",
                "hya/plan",
                "hya/workflow",
                "hya/multi-agent",
            ],
        },
        "network-tools" | "hya/network-tools" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/network"],
        },
        "channel-tools" | "hya/channel-tools" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/agent-channels"],
        },
        "goal-loop" | "hya/goal-loop" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/goal", "hya/loop"],
        },
        "plan-impl-review" | "hya/plan-impl-review" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/workflow"],
        },
        "code-mode" | "hya/code-mode" => BundleIdRemap::PromotedToCore {
            from: trimmed.to_owned(),
            note: "code-mode was promoted into hya-bare core; configure via `tools.code.mode`",
        },
        "compaction" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/compaction"],
        },
        "core-agents" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/core-agents"],
        },
        "core-skills" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/core-skills"],
        },
        "core-commands" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/core-commands"],
        },
        "agent-channels" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/agent-channels"],
        },
        "basic-tui-components" => BundleIdRemap::Remapped {
            from: trimmed.to_owned(),
            to: vec!["hya/basic-tui-components"],
        },
        other => BundleIdRemap::Unchanged(other.to_owned()),
    }
}

/// Remap a list of bundle IDs, preserving order and deduplicating results.
///
/// Returns `(remapped_ids, changed, notes)`.
#[must_use]
pub fn remap_bundle_id_list(ids: &[String]) -> (Vec<String>, bool, Vec<String>) {
    let mut out = Vec::with_capacity(ids.len());
    let mut seen = BTreeSet::new();
    let mut changed = false;
    let mut notes = Vec::new();
    for raw in ids {
        match remap_bundle_id(raw) {
            BundleIdRemap::Remapped { from, to } => {
                changed = true;
                notes.push(format!(
                    "remapped bundle id `{from}` -> [{}]",
                    to.join(", ")
                ));
                for target in to {
                    if seen.insert(target.to_owned()) {
                        out.push(target.to_owned());
                    }
                }
            }
            BundleIdRemap::PromotedToCore { from, note } => {
                changed = true;
                notes.push(format!("removed `{from}`: {note}"));
            }
            BundleIdRemap::Unchanged(id) => {
                if seen.insert(id.clone()) {
                    out.push(id);
                } else {
                    changed = true;
                }
            }
        }
    }
    (out, changed, notes)
}

/// Encode a bundle identity into its canonical percent-encoded leaf directory name,
/// matching `hya_app::bundle_config::bundle_config_location`.
#[must_use]
pub fn encode_bundle_leaf(bundle_id: &str) -> String {
    let special_dot = matches!(bundle_id, "." | "..");
    let mut encoded = String::with_capacity(bundle_id.len());
    for byte in bundle_id.as_bytes() {
        if !special_dot && is_unreserved(*byte) {
            encoded.push(char::from(*byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0F) as usize]));
        }
    }
    encoded
}

/// Compute `<config-dir>/bundles/<percent-encoded-bundle-id>/config.yml`.
#[must_use]
pub fn user_bundle_config_path(config_yaml: &Path, bundle_id: &str) -> PathBuf {
    let config_dir = config_yaml.parent().unwrap_or_else(|| Path::new("."));
    config_dir
        .join("bundles")
        .join(encode_bundle_leaf(bundle_id))
        .join("config.yml")
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}
