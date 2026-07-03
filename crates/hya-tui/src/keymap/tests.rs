use super::*;
use crate::contracts::{BindingId, Key, KeyEvent};
use std::time::{Duration, Instant};

fn key(key: Key) -> KeyEvent {
    KeyEvent::new(key)
}

fn ctrl(key: Key) -> KeyEvent {
    KeyEvent {
        ctrl: true,
        ..KeyEvent::new(key)
    }
}

#[test]
fn parses_ctrl_binding_when_spec_has_modifier() {
    let chords = parse_binding_spec("ctrl+p").expect("ctrl+p should parse");

    assert_eq!(chords.len(), 1);
    assert_eq!(chords[0].0, vec![ctrl(Key::Char('p'))]);
}

#[test]
fn parses_leader_binding_when_spec_has_token() {
    let chords = parse_binding_spec("<leader>e").expect("leader chord should parse");

    assert_eq!(chords.len(), 1);
    assert_eq!(chords[0].0, vec![ctrl(Key::Char('x')), key(Key::Char('e'))]);
}

#[test]
fn parses_alternative_bindings_when_spec_has_commas() {
    let chords = parse_binding_spec("a,b").expect("alternatives should parse");

    assert_eq!(chords.len(), 2);
    assert_eq!(chords[0].0, vec![key(Key::Char('a'))]);
    assert_eq!(chords[1].0, vec![key(Key::Char('b'))]);
}

#[test]
fn parses_shift_tab_when_spec_has_shift_modifier() {
    let chords = parse_binding_spec("shift+tab").expect("shift+tab should parse");

    assert_eq!(chords.len(), 1);
    assert_eq!(
        chords[0].0,
        vec![KeyEvent {
            key: Key::Tab,
            shift: true,
            ..KeyEvent::new(Key::Tab)
        }]
    );
}

#[test]
fn expands_key_aliases_when_spec_uses_origin_aliases() {
    let chords = parse_binding_spec("enter,esc,pgup,pgdn").expect("aliases should parse");

    assert_eq!(chords.len(), 4);
    assert_eq!(chords[0].0, vec![key(Key::Enter)]);
    assert_eq!(chords[1].0, vec![key(Key::Esc)]);
    assert_eq!(chords[2].0, vec![key(Key::PageUp)]);
    assert_eq!(chords[3].0, vec![key(Key::PageDown)]);
}

#[test]
fn dispatches_leader_chord_when_sequence_matches_default() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('x')), KeymapMode::Base),
        DispatchOutcome::Pending
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Char('l')), KeymapMode::Base),
        DispatchOutcome::Matched(BindingId("session.list".to_owned()))
    );
}

#[test]
fn default_leader_bindings_map_to_dialog_commands() {
    leader_dispatches_to(Key::Char('a'), "agent.list");
    leader_dispatches_to(Key::Char('m'), "model.list");
    leader_dispatches_to(Key::Char('t'), "theme.switch");
    leader_dispatches_to(Key::Char('n'), "session.new");
    leader_dispatches_to(Key::Char('y'), "messages.copy");
}

#[test]
fn command_palette_binding_dispatches_directly() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");
    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('p')), KeymapMode::Base),
        DispatchOutcome::Matched(BindingId("command.palette.show".to_owned()))
    );
}

fn leader_dispatches_to(second: Key, expected: &str) {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");
    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('x')), KeymapMode::Base),
        DispatchOutcome::Pending,
        "leader ctrl+x should be pending"
    );
    assert_eq!(
        dispatcher.dispatch(key(second), KeymapMode::Base),
        DispatchOutcome::Matched(BindingId(expected.to_owned())),
        "leader binding should map to {expected}"
    );
}

#[test]
fn clears_leader_pending_when_timeout_expires() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");
    let start = Instant::now();

    assert_eq!(
        dispatcher.dispatch_at(ctrl(Key::Char('x')), KeymapMode::Base, start),
        DispatchOutcome::Pending
    );
    assert_eq!(
        dispatcher.dispatch_at(
            key(Key::Char('l')),
            KeymapMode::Base,
            start + Duration::from_millis(DEFAULT_LEADER_TIMEOUT_MS + 1),
        ),
        DispatchOutcome::Unmatched
    );
}

#[test]
fn escape_clears_pending_sequence_without_dispatching_escape_binding() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('x')), KeymapMode::Base),
        DispatchOutcome::Pending
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Esc), KeymapMode::Base),
        DispatchOutcome::Cleared
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Char('l')), KeymapMode::Base),
        DispatchOutcome::Unmatched
    );
}

#[test]
fn backspace_pops_pending_sequence() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('x')), KeymapMode::Base),
        DispatchOutcome::Pending
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Backspace), KeymapMode::Base),
        DispatchOutcome::Cleared
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Char('l')), KeymapMode::Base),
        DispatchOutcome::Unmatched
    );
}

#[test]
fn dispatches_direct_binding_when_sequence_matches_default() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('p')), KeymapMode::Base),
        DispatchOutcome::Matched(BindingId("command.palette.show".to_owned()))
    );
}

#[test]
fn gates_default_bindings_by_current_mode() {
    let mut dispatcher = default_dispatcher().expect("default keymap should parse");

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('t')), KeymapMode::Modal),
        DispatchOutcome::Unmatched
    );
    assert_eq!(
        dispatcher.dispatch(key(Key::Down), KeymapMode::Modal),
        DispatchOutcome::Matched(BindingId("dialog.select.next".to_owned()))
    );
}

#[test]
fn picks_highest_priority_binding_when_chords_collide() {
    let leader = default_leader_key().expect("leader should parse");
    let chord = parse_binding_spec("ctrl+p")
        .expect("ctrl+p should parse")
        .remove(0);
    let low = KeyBinding {
        command: BindingId("low.priority".to_owned()),
        chord: chord.clone(),
        mode: Some(KeymapMode::Base),
        priority: 0,
    };
    let high = KeyBinding {
        command: BindingId("high.priority".to_owned()),
        chord,
        mode: Some(KeymapMode::Base),
        priority: 10,
    };
    let mut dispatcher = KeymapDispatcher::new(vec![low, high], leader, DEFAULT_LEADER_TIMEOUT_MS);

    assert_eq!(
        dispatcher.dispatch(ctrl(Key::Char('p')), KeymapMode::Base),
        DispatchOutcome::Matched(BindingId("high.priority".to_owned()))
    );
}

#[test]
fn default_catalog_contains_known_origin_mappings() {
    let bindings = default_bindings().expect("default keymap should parse");

    assert_eq!(command_mapping().len(), 164);
    assert_eq!(default_binding_specs().len(), 185);
    assert_eq!(
        canonical_command("dialog.select.next"),
        BindingId("dialog.select.next".to_owned())
    );
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "leader"
            && binding.command == BindingId("leader".to_owned())
            && binding.chords == parse_binding_spec("ctrl+x").expect("leader should parse")
    }));
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "command_list"
            && binding.command == BindingId("command.palette.show".to_owned())
    }));
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "session_list"
            && binding.command == BindingId("session.list".to_owned())
    }));
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "permission_yolo_switch"
            && binding.command == BindingId("permission.yolo.switch".to_owned())
            && !binding.enabled
    }));
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "editor_open"
            && binding.command == BindingId("prompt.editor".to_owned())
    }));
    assert!(bindings.iter().any(|binding| {
        binding.config_key == "theme_list"
            && binding.command == BindingId("theme.switch".to_owned())
    }));
    assert!(bindings
        .iter()
        .any(|binding| binding.config_key == "app_debug" && !binding.enabled));
}
