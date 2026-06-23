#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::{AppState, KeyBindingGroup, KeyBindingItem, KeyBindingsView};

#[test]
fn keybindings_overlay_renders_available_bindings_and_footer_hints() {
    // Given: a pending OpenCode-style key sequence exposes reachable bindings.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        keybindings: Some(KeyBindingsView {
            title: "Key Bindings".to_string(),
            groups: vec![
                KeyBindingGroup {
                    label: "System".to_string(),
                    items: vec![KeyBindingItem {
                        key: "s".to_string(),
                        label: "Status".to_string(),
                    }],
                },
                KeyBindingGroup {
                    label: "Session".to_string(),
                    items: vec![KeyBindingItem {
                        key: "n".to_string(),
                        label: "New session".to_string(),
                    }],
                },
                KeyBindingGroup {
                    label: "Model".to_string(),
                    items: vec![KeyBindingItem {
                        key: "m".to_string(),
                        label: "Select model".to_string(),
                    }],
                },
            ],
        }),
        ..AppState::default()
    };

    // When: the non-modal which-key overlay renders above the composer.
    let text = render(&mut state, 100, 24);

    // Then: it mirrors OpenCode's grouped keybinding surface.
    assert!(text.contains("Key Bindings"), "missing title:\n{text}");
    assert!(text.contains("System"), "missing System group:\n{text}");
    assert!(text.contains("Session"), "missing Session group:\n{text}");
    assert!(text.contains("Model"), "missing Model group:\n{text}");
    assert!(text.contains("Status"), "missing status binding:\n{text}");
    assert!(text.contains("s"), "missing status key:\n{text}");
    assert!(
        text.contains("esc dismiss") && text.contains("ctrl+p commands"),
        "missing OpenCode footer hints:\n{text}"
    );
    assert!(
        text.contains("Ask anything"),
        "which-key overlay should not hide the composer:\n{text}"
    );
    let hint_line = text
        .lines()
        .find(|line| line.contains("esc dismiss"))
        .unwrap_or_default();
    assert!(
        !hint_line.is_empty() && !hint_line.contains('▣') && !hint_line.contains("Sisyphus"),
        "which-key footer should clear the runtime strip behind it, got {hint_line:?}"
    );
}
