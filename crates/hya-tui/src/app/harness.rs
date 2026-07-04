//! Headless, in-process test harness for the default TUI.
//!
//! Drives a real [`Runtime`] against an in-memory `TestBackend` (via [`Tui::from_test_backend`]),
//! feeding synthetic [`AppEvent`]s and exposing both the rendered frame and the domain
//! `MessageStore` for assertions. It mirrors the legacy `hya-backend` `DummyHarness`, but drives
//! the *default* TUI's `handle_event`/`draw` loop instead of a `Controller`.
//!
//! Determinism: the harness drives `Runtime` step-by-step and never enters `run_tui`'s
//! `tokio::time::timeout` branches (leader chord / toast / spinner), so no wall-clock time is
//! involved. Tasks that `draw` spawns (e.g. yolo auto-approve) are flushed by [`AppHarness::settle`]
//! within a bounded loop.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use hya_sdk::{EventPayload, GlobalEvent, MessageStore};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::RwLock;

use super::runtime::{RunTuiInput, Runtime};
use super::AppEvent;
use crate::contracts::{Key, KeyEvent};
use crate::render::transcript::{format_store_transcript, TranscriptOptions};
use crate::state::AppState;
use crate::tui::Tui;

/// A headless driver around a real [`Runtime`].
pub(crate) struct AppHarness {
    runtime: Runtime,
    data: Arc<RwLock<MessageStore>>,
    #[allow(dead_code)]
    tx: UnboundedSender<AppEvent>,
    width: u16,
    height: u16,
}

impl AppHarness {
    /// Build a harness with a `width`x`height` in-memory terminal, an unconnected
    /// [`hya_sdk::PendingClient`] (so no network is touched), and a fresh session store. Renders
    /// the initial frame before returning.
    pub(crate) async fn new(width: u16, height: u16) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (client, _slot) = hya_sdk::PendingClient::create(".");
        let state = AppState::default();
        let data = Arc::clone(&state.data);
        let input = RunTuiInput {
            tui: Tui::from_test_backend(width, height).expect("test backend"),
            state,
            client,
            events: rx,
            tx: tx.clone(),
            input_task: None,
            default_agent: None,
            default_model: None,
            agent_names: Vec::new(),
        };
        let mut runtime = Runtime::new(input).expect("build runtime");
        runtime.draw().await.expect("initial draw");
        Self {
            runtime,
            data,
            tx,
            width,
            height,
        }
    }

    /// Feed one event through the same steps the real loop uses (`handle_event` ->
    /// `open_pending_editor`) and then settle.
    async fn dispatch(&mut self, event: AppEvent) {
        self.runtime
            .handle_event(event)
            .await
            .expect("handle event");
        self.runtime
            .open_pending_editor()
            .await
            .expect("open pending editor");
        self.settle().await;
    }

    /// Redraw and drain any events that spawned tasks pushed back onto the channel, bounded so a
    /// runaway never hangs the test.
    async fn settle(&mut self) {
        for _ in 0..64 {
            self.runtime.draw().await.expect("draw");
            tokio::task::yield_now().await;
            match self.runtime.try_next_event() {
                Some(event) => {
                    self.runtime
                        .handle_event(event)
                        .await
                        .expect("handle queued event");
                    self.runtime
                        .open_pending_editor()
                        .await
                        .expect("open pending editor");
                }
                None => return,
            }
        }
    }

    /// Press a single key (no modifiers).
    pub(crate) async fn press(&mut self, key: Key) {
        self.dispatch(AppEvent::Key(KeyEvent::new(key))).await;
    }

    /// Press a single character key with the Ctrl modifier (e.g. the leader key).
    pub(crate) async fn press_ctrl(&mut self, ch: char) {
        self.dispatch(AppEvent::Key(KeyEvent {
            ctrl: true,
            ..KeyEvent::new(Key::Char(ch))
        }))
        .await;
    }

    /// Type a run of characters one keypress at a time.
    pub(crate) async fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.press(Key::Char(ch)).await;
        }
    }

    /// Feed a team event as it reaches the frontend: wrapped in a `hya.envelope`
    /// global event whose `properties` carries the raw backend envelope.
    pub(crate) async fn push_team_event(&mut self, event: serde_json::Value) {
        self.push_sse(
            "hya.envelope",
            serde_json::json!({ "seq": 1, "event": event }),
        )
        .await;
    }

    /// Mark the backend ready so submitted prompts are sent (not queued).
    pub(crate) async fn backend_ready(&mut self) {
        self.dispatch(AppEvent::BackendReady).await;
    }

    /// Whether the main (input-bearing) pane is focused.
    pub(crate) fn focus_is_main(&self) -> bool {
        self.runtime.pane_focus_is_main()
    }

    /// The observed session ids of the open aux panes, in order.
    pub(crate) fn aux_sessions(&self) -> Vec<String> {
        self.runtime.aux_pane_sessions()
    }

    /// The main pane's route session id (what the input bar submits to).
    pub(crate) fn main_route_session(&self) -> Option<String> {
        self.runtime.main_route_session()
    }

    /// Whether `text` was submitted through the (main-only) input bar.
    pub(crate) fn submitted_prompt(&self, text: &str) -> bool {
        self.runtime.submitted_prompts().iter().any(|p| p == text)
    }

    /// Apply a server-sent event through the real `handle_event` path.
    pub(crate) async fn push_sse(&mut self, kind: &str, properties: serde_json::Value) {
        self.dispatch(AppEvent::Sse(global_event(kind, properties)))
            .await;
    }

    /// Navigate to a session route (as `AppEvent::Navigate` would from the backend).
    pub(crate) async fn navigate(&mut self, session_id: &str) {
        self.dispatch(AppEvent::Navigate(session_id.to_owned()))
            .await;
    }

    /// The rendered frame flattened to newline-joined rows.
    pub(crate) fn buffer_text(&self) -> String {
        let buffer = self
            .runtime
            .input
            .tui
            .test_buffer()
            .expect("test backend buffer");
        (0..self.height)
            .map(|row| {
                (0..self.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether the rendered frame contains `needle` on any single row.
    pub(crate) fn buffer_contains(&self, needle: &str) -> bool {
        self.buffer_text().lines().any(|line| line.contains(needle))
    }

    /// Read the domain `MessageStore` for assertions.
    pub(crate) async fn with_store<R>(&self, f: impl FnOnce(&MessageStore) -> R) -> R {
        f(&*self.data.read().await)
    }

    /// The markdown transcript the TUI would render for `session_id` (empty if unknown).
    pub(crate) async fn transcript(&self, session_id: &str) -> String {
        let store = self.data.read().await;
        format_store_transcript(&store, session_id, TranscriptOptions::default())
            .unwrap_or_default()
    }
}

fn global_event(kind: &str, properties: serde_json::Value) -> GlobalEvent {
    GlobalEvent {
        directory: None,
        project: None,
        workspace: None,
        payload: EventPayload {
            id: None,
            kind: kind.to_owned(),
            properties,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn home_screen_renders_connecting_placeholder() {
        let harness = AppHarness::new(80, 24).await;
        // The pending client never becomes ready, so the home prompt shows the connecting hint.
        // This proves the full Runtime -> AnyBackend::Test render path works headlessly.
        assert!(
            harness.buffer_contains("Starting backend"),
            "expected connecting placeholder, got frame:\n{}",
            harness.buffer_text()
        );
    }

    #[tokio::test]
    async fn typed_text_appears_in_prompt() {
        let mut harness = AppHarness::new(80, 24).await;
        harness.type_text("hello world").await;
        assert!(
            harness.buffer_contains("hello world"),
            "typed text should render in the prompt box; frame:\n{}",
            harness.buffer_text()
        );
    }

    #[tokio::test]
    async fn slash_autocomplete_opens_and_filters() {
        let mut harness = AppHarness::new(80, 24).await;
        harness.type_text("/").await;
        assert!(
            harness.buffer_contains("Slash commands"),
            "typing '/' should open the slash-autocomplete dropdown; frame:\n{}",
            harness.buffer_text()
        );
        assert!(
            harness.buffer_contains("model"),
            "dropdown should list builtin commands; frame:\n{}",
            harness.buffer_text()
        );

        harness.type_text("mo").await;
        // "/mo" filters the list down to the model commands.
        assert!(
            harness.buffer_contains("models"),
            "filtered dropdown should still contain 'models'; frame:\n{}",
            harness.buffer_text()
        );
        assert!(
            !harness.buffer_contains("sessions"),
            "filtered dropdown should drop non-matching commands; frame:\n{}",
            harness.buffer_text()
        );
    }

    #[tokio::test]
    async fn startup_resume_load_session_renders_resumed_transcript() {
        let mut h = AppHarness::new(100, 30).await;
        h.push_sse(
            "session.created",
            json!({ "info": { "id": "hysec_abcdefghijklmnopqrst" } }),
        )
        .await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_resume", "sessionID": "hysec_abcdefghijklmnopqrst", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_resume", "messageID": "msg_resume", "sessionID": "hysec_abcdefghijklmnopqrst", "type": "text", "text": "resumed transcript visible" } }),
        )
        .await;

        h.dispatch(AppEvent::LoadSession(
            "hysec_abcdefghijklmnopqrst".to_owned(),
        ))
        .await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("hysec_abcdefghijklmnopqrst")
        );
        assert!(
            h.buffer_contains("resumed transcript visible"),
            "resumed transcript should render after LoadSession; frame:\n{}",
            h.buffer_text()
        );
    }

    /// Seed a main session plus a subagent (`ses_child`) transcript and register it
    /// in the roster, then open the roster overlay and select it — leaving a focused,
    /// read-only aux pane on the child session.
    async fn open_child_aux(h: &mut AppHarness) {
        h.navigate("ses_main").await;
        h.push_sse("session.created", json!({ "info": { "id": "ses_child" } }))
            .await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_c", "sessionID": "ses_child", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_c", "messageID": "msg_c", "sessionID": "ses_child", "type": "text", "text": "child working" } }),
        )
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-3", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        // Leader chord (ctrl+x o) opens the roster; Enter selects the first entry.
        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        h.press(Key::Enter).await;
    }

    #[tokio::test]
    async fn roster_selection_opens_readonly_aux_pane() {
        let mut h = AppHarness::new(100, 30).await;
        h.navigate("ses_main").await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-3", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        h.push_sse("session.created", json!({ "info": { "id": "ses_child" } }))
            .await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_c", "sessionID": "ses_child", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_c", "messageID": "msg_c", "sessionID": "ses_child", "type": "text", "text": "child working" } }),
        )
        .await;

        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        assert!(
            h.buffer_contains("Team roster"),
            "leader+o opens the roster overlay; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            h.buffer_contains("reviewer-3"),
            "roster lists the registered agent; frame:\n{}",
            h.buffer_text()
        );

        h.press(Key::Enter).await;
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "selecting the roster entry opens a read-only pane on its session"
        );
        assert!(!h.focus_is_main(), "the new aux pane takes focus");
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "opening an aux pane must NOT change the main route/input target"
        );
        assert!(
            h.buffer_contains("read-only"),
            "the aux pane is labelled read-only; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            h.buffer_contains("child working"),
            "the aux pane renders the observed transcript live; frame:\n{}",
            h.buffer_text()
        );
    }

    #[tokio::test]
    async fn typing_while_aux_focused_routes_to_main_session() {
        // The hard input-routing invariant (decision 2): with an aux pane focused,
        // the bottom input bar STILL edits/submits the main session, never the aux.
        let mut h = AppHarness::new(100, 30).await;
        h.backend_ready().await;
        open_child_aux(&mut h).await;
        assert!(!h.focus_is_main(), "aux pane is focused for this test");

        h.type_text("hello main").await;
        h.press(Key::Enter).await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "the prompt targets the main session"
        );
        assert!(
            h.submitted_prompt("hello main"),
            "the prompt was submitted through the main input bar"
        );
        let aux_user_messages = h
            .with_store(|store| {
                store.messages.get("ses_child").map_or(0, |messages| {
                    messages
                        .iter()
                        .filter(|message| message.role.as_deref() == Some("user"))
                        .count()
                })
            })
            .await;
        assert_eq!(
            aux_user_messages, 0,
            "no user input ever reaches the observed (aux) session"
        );
    }

    #[tokio::test]
    async fn main_pane_uncloseable_and_aux_pane_is_readonly() {
        let mut h = AppHarness::new(100, 30).await;
        h.navigate("ses_main").await;

        // Closing while the main pane is focused is a no-op — main is uncloseable.
        h.press_ctrl('x').await;
        h.press(Key::Char('w')).await;
        assert!(h.focus_is_main(), "main pane stays after a close attempt");
        assert!(h.aux_sessions().is_empty(), "no panes were affected");

        open_child_aux(&mut h).await;
        assert!(!h.focus_is_main());
        let before = h
            .with_store(|store| store.messages.get("ses_child").map_or(0, Vec::len))
            .await;

        // A submit key while the aux pane is focused must not reach the aux session.
        h.type_text("noise").await;
        h.press(Key::Enter).await;
        let after = h
            .with_store(|store| store.messages.get("ses_child").map_or(0, Vec::len))
            .await;
        assert_eq!(
            after, before,
            "the read-only aux session received no messages"
        );

        // Aux panes ARE closeable; closing returns focus to the main pane.
        h.press_ctrl('x').await;
        h.press(Key::Char('w')).await;
        assert!(h.focus_is_main(), "closing the aux pane refocuses main");
        assert!(h.aux_sessions().is_empty(), "the aux pane was closed");
    }

    #[tokio::test]
    async fn sse_message_updates_store_and_renders_transcript() {
        let mut harness = AppHarness::new(100, 30).await;
        harness
            .push_sse("session.updated", json!({ "info": { "id": "ses_1" } }))
            .await;
        harness
            .push_sse(
                "message.updated",
                json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
            )
            .await;
        harness
            .push_sse(
                "message.part.updated",
                json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "pingpong" } }),
            )
            .await;
        harness.navigate("ses_1").await;

        // Domain store received the update.
        let message_count = harness
            .with_store(|store| store.messages.get("ses_1").map_or(0, Vec::len))
            .await;
        assert_eq!(message_count, 1, "store should hold the one user message");

        // The client-side transcript and the rendered frame both reflect it (no desync).
        let transcript = harness.transcript("ses_1").await;
        assert!(
            transcript.contains("pingpong"),
            "transcript should contain the message text; got:\n{transcript}"
        );
        assert!(
            harness.buffer_contains("pingpong"),
            "session screen should render the message text; frame:\n{}",
            harness.buffer_text()
        );
    }
}
