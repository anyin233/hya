//! Wire round-trip coverage for [`PluginKindWire`] kind tags.

use hya_plugin::messages::{PluginInfo, PluginKindWire};

#[test]
fn claude_kind_round_trips_on_the_wire() {
    for (kind, tag) in [
        (PluginKindWire::Rust, "rust"),
        (PluginKindWire::Bun, "bun"),
        (PluginKindWire::Claude, "claude"),
        (PluginKindWire::Other, "other"),
    ] {
        let encoded = serde_json::to_string(&kind).unwrap_or_else(|error| {
            panic!("serialize {tag} kind failed: {error}");
        });
        assert_eq!(
            encoded,
            format!("\"{tag}\""),
            "unexpected wire tag for {tag}"
        );
        let decoded: PluginKindWire = serde_json::from_str(&encoded).unwrap_or_else(|error| {
            panic!("deserialize {tag} kind failed: {error}");
        });
        assert_eq!(decoded, kind, "round-trip changed the {tag} kind");
    }
}

#[test]
fn claude_plugin_info_parses_from_initialize_reply_shape() {
    let info: PluginInfo =
        serde_json::from_str(r#"{"id":"claude-demo","version":"1.0.0","kind":"claude"}"#)
            .unwrap_or_else(|error| panic!("claude plugin info must parse: {error}"));
    assert_eq!(info.kind, PluginKindWire::Claude);
    assert_eq!(info.id, "claude-demo");

    let encoded = serde_json::to_string(&info).unwrap_or_else(|error| {
        panic!("reserialize plugin info failed: {error}");
    });
    assert!(
        encoded.contains(r#""kind":"claude""#),
        "reserialized info lost the claude kind tag: {encoded}"
    );
}
