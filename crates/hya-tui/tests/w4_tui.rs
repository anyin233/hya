use hya_sdk::{GlobalEvent, MessageStore};
use hya_tui::contracts::{Key, KeyEvent, PromptDoc, Rect, Rgba};
use hya_tui::render::text::{Line, Text};

#[test]
fn draw_color_when_alpha_present_blends_over_theme_background() {
    let color =
        hya_tui::render::draw::rgba_to_color(Rgba::new(255, 255, 255, 0x80), Rgba::rgb(0, 0, 0));

    assert_eq!(color, ratatui::style::Color::Rgb(128, 128, 128));
}

#[test]
fn tui_key_mapping_when_ctrl_char_pressed_preserves_contract_modifiers() {
    let event = ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Char('c'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    );

    assert_eq!(
        hya_tui::tui::map_key_event(event),
        KeyEvent {
            key: Key::Char('c'),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }
    );
}

#[test]
fn home_layout_when_terminal_is_wide_centers_the_prompt_box() {
    let layout = hya_tui::screens::home::compute_layout(
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        6,
    );

    assert_eq!(layout.prompt.width, 75);
    assert_eq!(layout.prompt.x, 12);
}

#[test]
fn prompt_request_body_when_doc_has_text_uses_real_sdk_parts_shape() {
    let doc = PromptDoc {
        text: "say hello".to_owned(),
        ..PromptDoc::default()
    };

    assert_eq!(
        hya_tui::app::prompt_request_body(&doc),
        serde_json::json!({"parts":[{"type":"text","text":"say hello"}]})
    );
}

#[test]
fn session_text_when_store_has_user_and_assistant_renders_both() {
    let event = |kind: &str, properties: serde_json::Value| -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .expect("decode event")
    };
    let mut store = MessageStore::default();
    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
    ));
    store.apply_event(&event(
        "message.part.updated",
        serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "say hello" } }),
    ));
    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_2", "sessionID": "ses_1", "role": "assistant", "time": { "created": 2 } } }),
    ));
    store.apply_event(&event(
        "message.part.updated",
        serde_json::json!({ "part": { "id": "prt_2", "messageID": "msg_2", "sessionID": "ses_1", "type": "text", "text": "hello **there**" } }),
    ));

    let theme = hya_tui::theme::resolve(
        &hya_tui::theme::builtin_theme(hya_tui::theme::DEFAULT_THEME)
            .expect("default theme parses")
            .expect("default theme exists"),
        hya_tui::theme::Mode::Dark,
    )
    .expect("default theme resolves");
    let text = hya_tui::screens::session::timeline_text(
        &store,
        "ses_1",
        &[],
        80,
        theme.border,
        &[],
        &[],
        "",
        false,
        &theme,
    );

    let rendered = flatten_text(&text.text);
    assert!(rendered.contains("say hello"), "user line: {rendered}");
    assert!(
        rendered.contains("hello there"),
        "assistant line: {rendered}"
    );
}

#[test]
fn session_text_when_prompt_queued_during_work_renders_queued_badge() {
    let event = |kind: &str, properties: serde_json::Value| -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .expect("decode event")
    };
    let mut store = MessageStore::default();
    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
    ));
    store.apply_event(&event(
        "message.part.updated",
        serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "first prompt" } }),
    ));
    assert!(
        store.is_working("ses_1"),
        "user-last session must be working"
    );

    let theme = hya_tui::theme::resolve(
        &hya_tui::theme::builtin_theme(hya_tui::theme::DEFAULT_THEME)
            .expect("default theme parses")
            .expect("default theme exists"),
        hya_tui::theme::Mode::Dark,
    )
    .expect("default theme resolves");
    let agent_color = theme.border;
    let pending = ["queued follow-up".to_string()];
    let text = hya_tui::screens::session::timeline_text(
        &store,
        "ses_1",
        &pending,
        80,
        agent_color,
        &[],
        &[],
        "",
        false,
        &theme,
    );

    let badge = text
        .text
        .0
        .iter()
        .flat_map(|line| line.0.iter())
        .find(|span| span.text == " QUEUED ")
        .expect("queued pending prompt renders a QUEUED badge");
    assert_eq!(badge.bg, Some(agent_color), "badge bg uses the agent color");
    assert!(badge.attrs.bold, "badge text is bold");

    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_2", "sessionID": "ses_1", "role": "assistant", "time": { "created": 2, "completed": 3 } } }),
    ));
    assert!(!store.is_working("ses_1"), "completed assistant => idle");
    let idle = hya_tui::screens::session::timeline_text(
        &store,
        "ses_1",
        &pending,
        80,
        agent_color,
        &[],
        &[],
        "",
        false,
        &theme,
    );
    let idle_text = flatten_text(&idle.text);
    assert!(
        idle_text.contains("queued follow-up"),
        "pending prompt still renders when idle: {idle_text}"
    );
    assert!(
        !idle_text.contains("QUEUED"),
        "no badge when the session is idle: {idle_text}"
    );
}

#[test]
fn session_text_when_reverted_hides_messages_and_shows_banner() {
    let event = |kind: &str, properties: serde_json::Value| -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .expect("decode event")
    };
    let mut store = MessageStore::default();
    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
    ));
    store.apply_event(&event(
        "message.part.updated",
        serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "first prompt" } }),
    ));
    store.apply_event(&event(
        "message.updated",
        serde_json::json!({ "info": { "id": "msg_3", "sessionID": "ses_1", "role": "user", "time": { "created": 4 } } }),
    ));
    store.apply_event(&event(
        "message.part.updated",
        serde_json::json!({ "part": { "id": "prt_3", "messageID": "msg_3", "sessionID": "ses_1", "type": "text", "text": "second prompt" } }),
    ));
    store.apply_event(&event(
        "session.updated",
        serde_json::json!({ "info": { "id": "ses_1", "revert": { "messageID": "msg_3" } } }),
    ));

    let theme = hya_tui::theme::resolve(
        &hya_tui::theme::builtin_theme(hya_tui::theme::DEFAULT_THEME)
            .expect("default theme parses")
            .expect("default theme exists"),
        hya_tui::theme::Mode::Dark,
    )
    .expect("default theme resolves");
    let pending = ["second prompt".to_string()];
    let text = hya_tui::screens::session::timeline_text(
        &store,
        "ses_1",
        &pending,
        80,
        theme.border,
        &[],
        &[],
        "",
        false,
        &theme,
    );
    let rendered = flatten_text(&text.text);

    assert!(
        rendered.contains("first prompt"),
        "kept turn still renders: {rendered}"
    );
    assert!(
        rendered.contains("1 message reverted"),
        "revert banner renders: {rendered}"
    );
    assert!(
        rendered.contains("/redo to restore"),
        "restore hint renders: {rendered}"
    );
    assert!(
        !rendered.contains("second prompt"),
        "reverted message stays hidden even as an optimistic echo: {rendered}"
    );
}

fn flatten_text(text: &Text) -> String {
    text.0
        .iter()
        .map(flatten_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn flatten_line(line: &Line) -> String {
    line.0.iter().map(|span| span.text.as_str()).collect()
}
