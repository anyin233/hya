#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::AppState;

use crate::config::ModelEntry;

use super::{Controller, TuiEffect};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

fn type_text(controller: &mut Controller, text: &str) {
    for ch in text.chars() {
        assert_eq!(
            controller.handle_key(key(KeyCode::Char(ch))),
            TuiEffect::None
        );
    }
}

#[test]
fn ctrl_p_enter_dispatches_selected_command() {
    // Given
    let mut controller =
        Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller
            .app
            .dialog
            .as_ref()
            .expect("model dialog after palette dispatch")
            .title,
        "select model"
    );
}

#[test]
fn ctrl_p_can_dispatch_non_dialog_commands_from_selection() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
    let new_index = controller
        .app
        .dialog
        .as_ref()
        .expect("command palette after ctrl-p")
        .items
        .iter()
        .position(|item| item.label == "/new" && !item.detail.starts_with("Suggested ·"))
        .expect("regular /new command in palette");
    for _ in 0..new_index {
        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    }
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then
    assert_eq!(effect, TuiEffect::NewSession);
    assert!(controller.app.dialog.is_none());
}

#[test]
fn slash_connect_opens_provider_setup_entry_when_no_models_exist() {
    // Given: the OpenCode-style empty home advertises /connect before models exist.
    let mut controller = Controller::new(AppState::default());

    // When: the user runs the advertised slash command.
    type_text(&mut controller, "/connect");
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then: it opens a visible setup entry instead of returning an unknown command.
    assert_eq!(effect, TuiEffect::None);
    let dialog = controller.app.dialog.as_ref().expect("connect dialog");
    assert_eq!(dialog.title, "Connect a provider");
    assert_eq!(dialog.items[0].label, "configure provider");
    assert!(
        dialog.items[0].detail.contains("configure"),
        "connect fallback should tell the user how to add a provider"
    );
}

#[test]
fn model_dialog_ctrl_a_opens_provider_list() {
    // Given: OpenCode model dialogs expose a ctrl+a action for provider setup.
    let mut controller = Controller::with_models_and_sessions(
        AppState::default(),
        vec![
            ModelEntry {
                id: "gpt-5".to_string(),
                provider: "openai".to_string(),
            },
            ModelEntry {
                id: "claude-sonnet".to_string(),
                provider: "anthropic".to_string(),
            },
            ModelEntry {
                id: "gpt-5".to_string(),
                provider: "anthropic".to_string(),
            },
            ModelEntry {
                id: "gpt-4o".to_string(),
                provider: "openai".to_string(),
            },
        ],
        Vec::new(),
    );

    // When: the user opens model selection and triggers the provider action.
    type_text(&mut controller, "/model");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("model dialog").title,
        "select model"
    );
    assert_eq!(controller.handle_key(ctrl('a')), TuiEffect::None);

    // Then: yaca shows the OpenCode-style provider list instead of ignoring the action.
    let dialog = controller.app.dialog.as_ref().expect("provider dialog");
    assert_eq!(dialog.title, "Connect a provider");
    let labels = dialog
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["anthropic", "openai"]);
}

#[test]
fn provider_dialog_enter_opens_filtered_model_list() {
    // Given: OpenCode returns to the selected provider's model list after provider setup.
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: "gpt-5".to_string(),
            model_provider_label: Some("openai".to_string()),
            ..AppState::default()
        },
        vec![
            ModelEntry {
                id: "gpt-5".to_string(),
                provider: "openai".to_string(),
            },
            ModelEntry {
                id: "claude-sonnet".to_string(),
                provider: "anthropic".to_string(),
            },
            ModelEntry {
                id: "gpt-5".to_string(),
                provider: "anthropic".to_string(),
            },
            ModelEntry {
                id: "gpt-4o".to_string(),
                provider: "openai".to_string(),
            },
        ],
        Vec::new(),
    );

    // When: the user opens /connect and selects the first configured provider.
    type_text(&mut controller, "/connect");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    // Then: yaca drills into that provider's filtered model list.
    let dialog = controller
        .app
        .dialog
        .as_ref()
        .expect("provider model dialog");
    assert_eq!(dialog.title, "anthropic");
    assert_eq!(dialog.subtitle, "select a model from this provider");
    let labels = dialog
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["claude-sonnet", "gpt-5"]);
    assert_eq!(dialog.selected, 0);
    assert_eq!(dialog.items[1].detail, "anthropic");

    // When: the user confirms the second filtered model.
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then: the selected model keeps the filtered provider identity.
    assert_eq!(
        effect,
        TuiEffect::SelectModel {
            model: "gpt-5".to_string(),
            provider: Some("anthropic".to_string()),
        }
    );
    assert_eq!(controller.app.model, "gpt-5");
    assert_eq!(
        controller.app.model_provider_label.as_deref(),
        Some("anthropic")
    );
}

#[test]
fn ctrl_p_palette_prepends_suggested_commands_with_category() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);

    // Then
    let dialog = controller
        .app
        .dialog
        .as_ref()
        .expect("command palette after ctrl-p");
    assert_eq!(dialog.items[0].label, "/model");
    assert!(
        dialog.items[0].detail.starts_with("Suggested ·"),
        "first palette row should be an OpenCode-style Suggested command: {:?}",
        dialog.items[0]
    );
    let regular_model = dialog
        .items
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, item)| item.label == "/model")
        .expect("regular /model command remains in full command list");
    assert!(
        regular_model.1.detail.starts_with("Agent ·"),
        "regular command should keep its own category: {:?}",
        regular_model.1
    );
}
