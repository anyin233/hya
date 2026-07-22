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
    /// The current multi-pane placement mode.
    pub(crate) fn pane_layout_kind(&self) -> super::panes::PaneLayoutKind {
        self.runtime.pane_layout_kind()
    }

    /// The live Subagent-manager state when the roster dialog is open.
    pub(crate) fn roster_dialog_state(&self) -> Option<super::runtime::RosterDialogState> {
        self.runtime.roster_dialog_state()
    }

    /// The main pane's route session id (what the input bar submits to).
    pub(crate) fn main_route_session(&self) -> Option<String> {
        self.runtime.main_route_session()
    }

    /// Whether `text` was submitted through the (main-only) input bar.
    pub(crate) fn submitted_prompt(&self, text: &str) -> bool {
        self.runtime.submitted_prompts().iter().any(|p| p == text)
    }

    /// The main prompt composer's current text.
    pub(crate) fn prompt_text(&self) -> &str {
        self.runtime.prompt_text()
    }

    /// The selected built-in theme name after client-side theme changes.
    pub(crate) fn active_theme_name(&self) -> &str {
        self.runtime.active_theme_name()
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
    use super::super::panes::PaneLayoutKind;
    use super::*;
    use async_trait::async_trait;
    use hya_sdk::{ApiClient, Client, SdkError, Transport};
    use serde_json::json;
    use serde_json::Value;
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use tokio::sync::{Notify, Semaphore};

    #[derive(Debug)]
    struct RecordingTransportState {
        next_session_ids: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<(String, String, Option<Value>)>>,
        session_create_started: AtomicUsize,
        block_session_create: AtomicBool,
        session_create_releases: Mutex<Vec<Arc<Semaphore>>>,
        next_session_create_release_ordinal: AtomicUsize,
        session_create_error: Mutex<Option<String>>,
        abort_started: AtomicUsize,
        block_abort: AtomicBool,
        abort_error: Mutex<Option<String>>,
        abort_release: Notify,
    }

    impl RecordingTransportState {
        fn new(next_session_id: &str) -> Self {
            Self {
                next_session_ids: Mutex::new(VecDeque::from([next_session_id.to_owned()])),
                requests: Mutex::new(Vec::new()),
                session_create_started: AtomicUsize::new(0),
                block_session_create: AtomicBool::new(false),
                session_create_releases: Mutex::new(Vec::new()),
                next_session_create_release_ordinal: AtomicUsize::new(0),
                session_create_error: Mutex::new(None),
                abort_started: AtomicUsize::new(0),
                block_abort: AtomicBool::new(false),
                abort_error: Mutex::new(None),
                abort_release: Notify::new(),
            }
        }

        fn count_requests(&self, path: &str) -> usize {
            self.requests
                .lock()
                .expect("request log")
                .iter()
                .filter(|(_, candidate, _)| candidate == path)
                .count()
        }
        fn count_method_requests(&self, method: &str, path: &str) -> usize {
            self.requests
                .lock()
                .expect("request log")
                .iter()
                .filter(|(candidate_method, candidate_path, _)| {
                    candidate_method == method && candidate_path == path
                })
                .count()
        }
        fn request_bodies(&self, method: &str, path: &str) -> Vec<Value> {
            self.requests
                .lock()
                .expect("request log")
                .iter()
                .filter_map(|(candidate_method, candidate_path, candidate_body)| {
                    (candidate_method == method && candidate_path == path)
                        .then(|| candidate_body.clone())
                        .flatten()
                })
                .collect()
        }

        fn abort_started(&self) -> usize {
            self.abort_started.load(Ordering::SeqCst)
        }
        fn queue_session_create_id(&self, session_id: &str) {
            self.next_session_ids
                .lock()
                .expect("session create ids")
                .push_back(session_id.to_owned());
        }
        fn block_session_create(&self) {
            self.block_session_create.store(true, Ordering::SeqCst);
        }

        fn release_session_create(&self) {
            let ordinal = self
                .next_session_create_release_ordinal
                .fetch_add(1, Ordering::SeqCst)
                + 1;
            self.release_session_create_ordinal(ordinal);
        }
        fn release_session_create_ordinal(&self, ordinal: usize) {
            let release = self
                .session_create_releases
                .lock()
                .expect("session create releases")
                .get(
                    ordinal
                        .checked_sub(1)
                        .expect("session create ordinals start at 1"),
                )
                .cloned()
                .unwrap_or_else(|| panic!("session create #{ordinal} not started"));
            release.add_permits(1);
        }
        fn fail_session_create(&self, message: &str) {
            *self
                .session_create_error
                .lock()
                .expect("session create error") = Some(message.to_owned());
        }

        async fn wait_for_session_create_started(&self, target: usize) {
            while self.session_create_started.load(Ordering::SeqCst) < target {
                tokio::task::yield_now().await;
            }
        }

        fn block_abort(&self) {
            self.block_abort.store(true, Ordering::SeqCst);
        }

        fn release_abort(&self) {
            self.block_abort.store(false, Ordering::SeqCst);
            self.abort_release.notify_waiters();
        }

        fn fail_abort(&self, message: &str) {
            *self.abort_error.lock().expect("abort error") = Some(message.to_owned());
        }
    }

    #[derive(Clone, Debug)]
    struct RecordingTransport {
        state: Arc<RecordingTransportState>,
    }

    #[async_trait]
    impl Transport for RecordingTransport {
        fn base_url(&self) -> &str {
            "http://test.invalid"
        }

        fn directory(&self) -> &str {
            "."
        }

        async fn request(
            &self,
            method: &str,
            path: &str,
            body: Option<&Value>,
        ) -> Result<Value, SdkError> {
            self.state.requests.lock().expect("request log").push((
                method.to_owned(),
                path.to_owned(),
                body.cloned(),
            ));
            match (method, path) {
                ("POST", "/session") => {
                    let (release, session_id) = {
                        let mut releases = self
                            .state
                            .session_create_releases
                            .lock()
                            .expect("session create releases");
                        let mut ids = self
                            .state
                            .next_session_ids
                            .lock()
                            .expect("session create ids");
                        let release = Arc::new(Semaphore::new(0));
                        releases.push(Arc::clone(&release));
                        let started = self
                            .state
                            .session_create_started
                            .fetch_add(1, Ordering::SeqCst)
                            + 1;
                        debug_assert_eq!(
                            releases.len(),
                            started,
                            "session create releases should track request ordinals in start order"
                        );
                        let session_id = if ids.len() > 1 {
                            ids.pop_front().expect("queued session create id")
                        } else {
                            ids.front().cloned().expect("default session create id")
                        };
                        (release, session_id)
                    };
                    if self.state.block_session_create.load(Ordering::SeqCst) {
                        release
                            .acquire()
                            .await
                            .expect("session create release permit")
                            .forget();
                    }
                    if let Some(message) = self
                        .state
                        .session_create_error
                        .lock()
                        .expect("session create error")
                        .clone()
                    {
                        return Err(SdkError::Http(message));
                    }
                    Ok(json!({ "id": session_id }))
                }
                (_, abort_path) if abort_path.ends_with("/abort") => {
                    self.state.abort_started.fetch_add(1, Ordering::SeqCst);
                    if self.state.block_abort.load(Ordering::SeqCst) {
                        self.state.abort_release.notified().await;
                    }
                    if let Some(message) =
                        self.state.abort_error.lock().expect("abort error").clone()
                    {
                        return Err(SdkError::Http(message));
                    }
                    Ok(Value::Null)
                }
                _ => Ok(Value::Null),
            }
        }
    }

    fn recording_client(next_session_id: &str) -> (Arc<RecordingTransportState>, Arc<dyn Client>) {
        let state = Arc::new(RecordingTransportState::new(next_session_id));
        let client: Arc<dyn Client> = Arc::new(ApiClient::with_transport(RecordingTransport {
            state: Arc::clone(&state),
        }));
        (state, client)
    }

    async fn harness_with_client(width: u16, height: u16, client: Arc<dyn Client>) -> AppHarness {
        let (tx, rx) = mpsc::unbounded_channel();
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
        AppHarness {
            runtime,
            data,
            tx,
            width,
            height,
        }
    }

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
    async fn main_agent_session_status_renders_opencode_indicator_at_bottom_left() {
        let mut harness = AppHarness::new(80, 24).await;
        harness
            .push_sse("session.created", json!({ "info": { "id": "ses_main" } }))
            .await;
        harness.navigate("ses_main").await;

        harness
            .push_sse(
                "session.status",
                json!({ "sessionID": "ses_main", "status": { "type": "busy" } }),
            )
            .await;

        let frame = harness.buffer_text();
        let bottom = frame.lines().last().expect("bottom row");
        assert!(
            bottom.contains("esc interrupt"),
            "a busy main agent should show the OpenCode interrupt indicator; frame:\n{frame}"
        );
        assert_eq!(
            bottom.chars().position(|cell| !cell.is_whitespace()),
            Some(1),
            "the running spinner should be the bottom-left indicator; row: {bottom}"
        );
        assert!(
            !bottom.contains("Running"),
            "the OpenCode indicator should not add a Running label; row: {bottom}"
        );

        harness
            .push_sse(
                "session.status",
                json!({ "sessionID": "ses_main", "status": { "type": "idle" } }),
            )
            .await;

        assert!(
            !harness.buffer_contains("esc interrupt"),
            "the running indicator should clear when the main agent becomes idle; frame:\n{}",
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
    async fn slash_themes_opens_picker_marks_current_and_applies_selection() {
        let mut harness = AppHarness::new(80, 40).await;

        harness.type_text("/themes").await;
        harness.press(Key::Enter).await;

        assert!(
            harness.buffer_contains("Themes"),
            "/themes should open the built-in theme picker; frame:\n{}",
            harness.buffer_text()
        );
        assert!(
            harness.buffer_contains("hya") && harness.buffer_contains("current"),
            "theme picker should mark the active theme; frame:\n{}",
            harness.buffer_text()
        );

        harness.press(Key::Down).await;
        harness.press(Key::Enter).await;

        assert_ne!(harness.active_theme_name(), "hya");
        assert!(
            !harness.buffer_contains("Themes"),
            "selecting a theme should close the picker; frame:\n{}",
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

    /// Seed a main session transcript, plus a child subagent transcript and roster entry.
    async fn seed_main_and_child_roster(h: &mut AppHarness) {
        h.navigate("ses_main").await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_main", "sessionID": "ses_main", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_main", "messageID": "msg_main", "sessionID": "ses_main", "type": "text", "text": "main working" } }),
        )
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
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-3", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
    }

    async fn seed_main_root_and_child_roster(h: &mut AppHarness) {
        seed_main_and_child_roster(h).await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_main",
            "handle": "main", "agent_type": "main", "mode": "resident"
        }))
        .await;
    }

    async fn seed_main_child_and_sibling_roster(h: &mut AppHarness) {
        seed_main_and_child_roster(h).await;
        h.push_sse(
            "session.created",
            json!({ "info": { "id": "ses_sibling" } }),
        )
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_sibling",
            "handle": "reviewer-4", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
    }

    async fn press_shift_char(h: &mut AppHarness, ch: char) {
        h.dispatch(AppEvent::Key(KeyEvent {
            shift: true,
            ..KeyEvent::new(Key::Char(ch))
        }))
        .await;
    }

    /// Seed a main session plus a subagent (`ses_child`) transcript and register it
    /// in the Roster, then open the Subagent manager and select it — leaving a focused,
    /// read-only Subagent observation view on the child Session.
    async fn open_child_aux(h: &mut AppHarness) {
        seed_main_and_child_roster(h).await;
        // Leader chord (ctrl+x o) opens the Subagent manager; Enter selects the first entry.
        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        h.press(Key::Enter).await;
    }

    #[tokio::test]
    async fn main_view_status_indicator_uses_roster_live_count_and_attention() {
        let mut h = AppHarness::new(100, 30).await;
        seed_main_and_child_roster(&mut h).await;

        assert!(
            h.focus_is_main(),
            "the main Session should stay focused for the status indicator"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "the main Session route should stay on the parent session"
        );
        assert!(
            h.buffer_contains("1 subagent"),
            "the main Session should show the live roster count even when the child session has no parentID; frame:\n{}",
            h.buffer_text()
        );

        h.push_sse(
            "permission.asked",
            json!({
                "id": "per_child",
                "sessionID": "ses_child",
                "permission": "edit",
                "patterns": ["src/main.rs"],
                "metadata": { "filepath": "src/main.rs" },
                "always": []
            }),
        )
        .await;

        assert!(
            h.buffer_contains("1 attention"),
            "the main Session should show attention for a roster child with a pending permission request; frame:\n{}",
            h.buffer_text()
        );
    }

    #[tokio::test]
    async fn escape_returns_from_parentless_roster_child_to_team_root() {
        let mut h = AppHarness::new(100, 30).await;
        seed_main_and_child_roster(&mut h).await;
        h.navigate("ses_child").await;

        h.press(Key::Esc).await;

        assert_eq!(h.main_route_session().as_deref(), Some("ses_main"));
    }

    #[tokio::test]
    async fn escape_returns_after_failed_split_from_parentless_roster_child() {
        let mut h = AppHarness::new(100, 30).await;
        seed_main_and_child_roster(&mut h).await;
        h.navigate("ses_child").await;

        h.press_ctrl('x').await;
        press_shift_char(&mut h, 'v').await;
        h.press(Key::Enter).await;

        assert!(h.aux_sessions().is_empty());

        h.press(Key::Esc).await;

        assert_eq!(h.main_route_session().as_deref(), Some("ses_main"));
    }

    #[tokio::test]
    async fn escape_returns_from_child_backed_vertical_split_to_team_root() {
        let mut h = AppHarness::new(120, 30).await;
        seed_main_child_and_sibling_roster(&mut h).await;
        h.navigate("ses_child").await;

        h.press_ctrl('x').await;
        press_shift_char(&mut h, 'v').await;
        h.press(Key::Enter).await;

        assert_eq!(h.aux_sessions(), vec!["ses_sibling".to_owned()]);
        assert_eq!(h.pane_layout_kind(), PaneLayoutKind::VerticalSplit);
        assert!(!h.focus_is_main());

        h.press(Key::Esc).await;

        assert!(h.focus_is_main());
        assert_eq!(h.main_route_session().as_deref(), Some("ses_main"));
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
            h.buffer_contains("Subagent manager"),
            "leader+o opens the Subagent manager; frame:\n{}",
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
    async fn roster_selection_does_not_open_main_session_as_aux_pane() {
        let mut h = AppHarness::new(100, 30).await;
        seed_main_root_and_child_roster(&mut h).await;

        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        assert!(
            h.buffer_contains("Subagent manager"),
            "leader+o opens the Subagent manager; frame:\n{}",
            h.buffer_text()
        );

        let roster = h
            .roster_dialog_state()
            .expect("Subagent manager should expose live roster state");
        assert!(
            roster
                .item_sessions
                .iter()
                .any(|session| session == "ses_child"),
            "the roster should still expose the real child session; state: {roster:?}"
        );
        assert!(
            h.aux_sessions().iter().all(|session| session != "ses_main"),
            "opening the manager alone must not create a main-session aux pane"
        );

        if roster
            .item_sessions
            .iter()
            .any(|session| session == "ses_main")
        {
            if roster.selected_session.as_deref() != Some("ses_main") {
                h.press(Key::Up).await;
            }
            let selected = h
                .roster_dialog_state()
                .expect("Subagent manager should remain open while checking root selection")
                .selected_session;
            if selected.as_deref() == Some("ses_main") {
                h.press(Key::Enter).await;
                assert!(
                    h.aux_sessions().iter().all(|session| session != "ses_main"),
                    "pressing Enter on the main/root roster row must not open ses_main as a read-only aux pane"
                );
                assert_eq!(
                    h.main_route_session().as_deref(),
                    Some("ses_main"),
                    "the main route/input target must stay on ses_main"
                );
                return;
            }
        }

        h.press(Key::Enter).await;
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "default roster selection should skip the main/root row and open the real child session"
        );
        assert!(
            h.aux_sessions().iter().all(|session| session != "ses_main"),
            "the Subagent manager must never open ses_main as a read-only aux pane"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "opening a child aux pane must not retarget the main input route"
        );
    }

    #[tokio::test]
    async fn child_route_roster_uses_root_team_scope_and_skips_self_and_main_row() {
        let mut h = AppHarness::new(120, 30).await;
        h.push_sse(
            "session.created",
            json!({ "info": { "id": "ses_child", "parentID": "ses_main" } }),
        )
        .await;
        h.push_sse(
            "session.created",
            json!({ "info": { "id": "ses_sibling", "parentID": "ses_main" } }),
        )
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_main",
            "handle": "main", "agent_type": "main", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-1", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_sibling",
            "handle": "reviewer-2", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;

        h.navigate("ses_child").await;
        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;

        assert!(
            h.buffer_contains("Subagent manager"),
            "leader+o opens the Subagent manager from a child route; frame:\n{}",
            h.buffer_text()
        );

        let roster = h
            .roster_dialog_state()
            .expect("Subagent manager should expose live roster state on a child route");
        assert_eq!(
            roster.item_sessions,
            vec!["ses_sibling".to_owned()],
            "a child route should resolve the root Team, list only sibling subagents, and skip both itself and the root/main row; state: {roster:?}"
        );
        assert_eq!(
            roster.selected_session.as_deref(),
            Some("ses_sibling"),
            "the sibling subagent should be the only selectable roster target from a child route"
        );
    }

    #[tokio::test]
    async fn typing_while_subagent_observation_view_is_focused_is_ignored() {
        let mut h = AppHarness::new(100, 30).await;
        h.backend_ready().await;
        open_child_aux(&mut h).await;
        assert!(
            !h.focus_is_main(),
            "the Subagent observation view is focused for this test"
        );
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "the focused aux pane observes the child Subagent session"
        );

        let typed = "ignore this";
        let prompt_before = h.prompt_text().to_owned();

        h.type_text(typed).await;

        assert_eq!(
            h.prompt_text(),
            prompt_before,
            "ordinary text typed into a focused Subagent observation view must not reach the main Prompt composer"
        );
        assert!(
            !h.buffer_contains(typed),
            "ignored text must not render in the main prompt; frame:\n{}",
            h.buffer_text()
        );

        h.press(Key::Enter).await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "ignoring aux-focused typing must not retarget the main session route"
        );
        assert_eq!(
            h.prompt_text(),
            prompt_before,
            "pressing Enter from a focused Subagent observation view must leave the main Prompt composer unchanged"
        );
        assert!(
            !h.submitted_prompt(typed),
            "ignored aux-focused text must not be recorded as a submitted prompt"
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
            "the observed child Session stays read-only and receives no user messages"
        );
    }

    #[tokio::test]
    async fn aux_transcript_new_output_indicator_persists_until_returning_to_bottom() {
        let mut h = AppHarness::new(100, 12).await;
        open_child_aux(&mut h).await;

        assert!(
            !h.focus_is_main(),
            "the Subagent observation view is focused for aux-scroll coverage"
        );

        for index in 0..18 {
            let msg_id = format!("msg_fill_{index}");
            h.push_sse(
                "message.updated",
                json!({ "info": {
                    "id": msg_id.clone(),
                    "sessionID": "ses_child",
                    "role": "assistant",
                    "time": { "created": index + 2 }
                } }),
            )
            .await;
            h.push_sse(
                "message.part.updated",
                json!({ "part": {
                    "id": format!("prt_fill_{index}"),
                    "messageID": msg_id,
                    "sessionID": "ses_child",
                    "type": "text",
                    "text": format!("fill line {index}")
                } }),
            )
            .await;
        }

        h.press(Key::PageUp).await;
        h.press(Key::PageUp).await;

        let newest_marker = "brand_new_tail_marker";
        h.push_sse(
            "message.updated",
            json!({ "info": {
                "id": "msg_tail",
                "sessionID": "ses_child",
                "role": "assistant",
                "time": { "created": 99 }
            } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": {
                "id": "prt_tail",
                "messageID": "msg_tail",
                "sessionID": "ses_child",
                "type": "text",
                "text": newest_marker
            } }),
        )
        .await;

        assert!(
            h.buffer_contains("new output"),
            "manual aux scrolling should pin the view and surface a new output indicator; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            !h.buffer_contains(newest_marker),
            "a manually pinned aux pane should stay above bottom until the user returns there; frame:\n{}",
            h.buffer_text()
        );

        h.settle().await;
        assert!(
            h.buffer_contains("new output"),
            "the aux new-output indicator must persist after render instead of disappearing after one frame; frame:\n{}",
            h.buffer_text()
        );

        h.press(Key::End).await;
        assert!(
            !h.buffer_contains("new output"),
            "returning the focused aux transcript to bottom should clear the new-output indicator; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            h.buffer_contains(newest_marker),
            "returning to bottom should reveal the newest child output; frame:\n{}",
            h.buffer_text()
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
    async fn issue14_subagent_manager_v_and_s_request_split_placement() {
        for (key, expected) in [
            ('v', PaneLayoutKind::VerticalSplit),
            ('s', PaneLayoutKind::HorizontalSplit),
        ] {
            let mut h = AppHarness::new(120, 30).await;
            seed_main_and_child_roster(&mut h).await;

            h.press_ctrl('x').await;
            h.press(Key::Char('o')).await;
            assert!(
                h.buffer_contains("Subagent manager"),
                "ctrl+x o should open the Subagent manager before '{key}'; frame:\n{}",
                h.buffer_text()
            );

            h.press(Key::Char(key)).await;

            assert_eq!(
                h.aux_sessions(),
                vec!["ses_child".to_owned()],
                "manager '{key}' should immediately open the selected child session"
            );
            assert!(
                !h.focus_is_main(),
                "manager '{key}' should focus the new Subagent observation view"
            );
            assert_eq!(
                h.main_route_session().as_deref(),
                Some("ses_main"),
                "manager '{key}' must not retarget the main input session"
            );
            assert_eq!(
                h.pane_layout_kind(),
                expected,
                "manager '{key}' should request the expected split placement"
            );
            assert!(
                h.buffer_contains("main working"),
                "manager '{key}' split should keep the main transcript visible; frame:\n{}",
                h.buffer_text()
            );
            assert!(
                h.buffer_contains("child working"),
                "manager '{key}' split should render the child transcript; frame:\n{}",
                h.buffer_text()
            );
        }
    }

    #[tokio::test]
    async fn issue14_subagent_direct_shortcuts_preselect_requested_split() {
        for (shortcut, expected) in [
            ('v', PaneLayoutKind::VerticalSplit),
            ('s', PaneLayoutKind::HorizontalSplit),
        ] {
            let mut h = AppHarness::new(120, 30).await;
            seed_main_and_child_roster(&mut h).await;

            assert_eq!(h.pane_layout_kind(), PaneLayoutKind::MainOnly);
            h.press_ctrl('x').await;
            press_shift_char(&mut h, shortcut).await;

            assert_eq!(
                h.pane_layout_kind(),
                PaneLayoutKind::MainOnly,
                "ctrl+x Shift+{shortcut} should only open the selector before Enter"
            );
            assert!(
                h.buffer_contains("Subagent manager"),
                "ctrl+x Shift+{shortcut} should open the placement-aware selector; frame:\n{}",
                h.buffer_text()
            );

            h.press(Key::Enter).await;

            assert_eq!(
                h.aux_sessions(),
                vec!["ses_child".to_owned()],
                "ctrl+x Shift+{shortcut} Enter should open the selected child session"
            );
            assert_eq!(
                h.pane_layout_kind(),
                expected,
                "ctrl+x Shift+{shortcut} Enter should commit the preselected placement"
            );
        }
    }

    #[tokio::test]
    async fn issue14_subagent_explicit_split_key_overrides_preselected_shortcut() {
        let mut h = AppHarness::new(120, 30).await;
        seed_main_and_child_roster(&mut h).await;

        h.press_ctrl('x').await;
        press_shift_char(&mut h, 'v').await;
        assert!(
            h.buffer_contains("Subagent manager"),
            "ctrl+x Shift+v should open the placement-aware selector before override; frame:\n{}",
            h.buffer_text()
        );

        h.press(Key::Char('s')).await;
        h.press(Key::Enter).await;

        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "overriding the preselected placement should still open the selected child session"
        );
        assert_eq!(
            h.pane_layout_kind(),
            PaneLayoutKind::HorizontalSplit,
            "explicit 's' should override ctrl+x Shift+v preselection"
        );
    }

    #[tokio::test]
    async fn issue14_open_roster_dialog_stays_live_across_team_updates() {
        let mut h = AppHarness::new(120, 30).await;
        h.navigate("ses_main").await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_main", "sessionID": "ses_main", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_main", "messageID": "msg_main", "sessionID": "ses_main", "type": "text", "text": "main working" } }),
        )
        .await;
        h.push_sse("session.created", json!({ "info": { "id": "ses_alpha" } }))
            .await;
        h.push_sse("session.created", json!({ "info": { "id": "ses_child" } }))
            .await;
        h.push_sse(
            "message.updated",
            json!({ "info": { "id": "msg_child", "sessionID": "ses_child", "role": "assistant", "time": { "created": 1 } } }),
        )
        .await;
        h.push_sse(
            "message.part.updated",
            json!({ "part": { "id": "prt_child", "messageID": "msg_child", "sessionID": "ses_child", "type": "text", "text": "child working" } }),
        )
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_alpha",
            "handle": "reviewer-1", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-3", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;

        h.press_ctrl('x').await;
        press_shift_char(&mut h, 'v').await;
        h.press(Key::Char('/')).await;
        h.type_text("reviewer-").await;
        h.press(Key::Down).await;

        let before = h
            .roster_dialog_state()
            .expect("Subagent manager should stay open while filtering");
        assert_eq!(before.filter, "reviewer-");
        assert!(before.filtering, "'/' should leave roster filtering active");
        assert_eq!(before.placement, Some(PaneLayoutKind::VerticalSplit));
        assert_eq!(
            before.selected_session.as_deref(),
            Some("ses_child"),
            "the focused roster entry before the SSE update should be the child session"
        );
        assert_eq!(
            before.item_sessions,
            vec!["ses_alpha".to_owned(), "ses_child".to_owned()],
            "the filtered roster should start with the two matching registered agents"
        );

        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_beta",
            "handle": "reviewer-2", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "agent_activity_changed", "session": "ses_main", "handle": "reviewer-3",
            "status": "busy", "current_task": "triaging"
        }))
        .await;

        assert!(
            h.buffer_contains("Subagent manager"),
            "team SSE updates should not close the open roster dialog; frame:\n{}",
            h.buffer_text()
        );

        let after = h
            .roster_dialog_state()
            .expect("Subagent manager should remain open after the SSE update");
        assert_eq!(after.filter, "reviewer-");
        assert!(
            after.filtering,
            "the roster should stay in filtering mode after SSE"
        );
        assert_eq!(
            after.placement,
            Some(PaneLayoutKind::VerticalSplit),
            "the preselected split placement should survive SSE refreshes"
        );
        assert_eq!(
            after.selected_session.as_deref(),
            Some("ses_child"),
            "refreshing the roster should keep the same selected session when it still exists"
        );
        assert_eq!(
            after.item_sessions,
            vec![
                "ses_alpha".to_owned(),
                "ses_beta".to_owned(),
                "ses_child".to_owned(),
            ],
            "the filtered roster should refresh to include the new matching agent"
        );
        assert!(
            after
                .item_titles
                .iter()
                .any(|title| title.contains("reviewer-2")),
            "the refreshed roster items should include the newly registered agent"
        );
        assert!(
            after
                .item_titles
                .iter()
                .any(|title| title.contains("reviewer-3")
                    && title.contains("busy")
                    && title.contains("triaging")),
            "the refreshed roster items should include updated live status/task metadata"
        );

        h.press(Key::Enter).await;

        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "Enter should still open the session that stayed selected across the SSE refresh"
        );
        assert_eq!(
            h.pane_layout_kind(),
            PaneLayoutKind::VerticalSplit,
            "the retained roster placement should still apply when opening the aux pane"
        );
    }

    #[tokio::test]
    async fn lifecycle_auto_close_done_status_closes_aux_without_removing_roster_entry() {
        let mut h = AppHarness::new(120, 30).await;
        open_child_aux(&mut h).await;

        assert_eq!(h.aux_sessions(), vec!["ses_child".to_owned()]);
        assert!(
            !h.focus_is_main(),
            "the child observation pane starts focused"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        assert!(
            h.focus_is_main(),
            "leader+0 should move focus back to the main pane before the lifecycle event"
        );
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "focusing main should not close the observed child pane"
        );

        h.push_team_event(json!({
            "type": "agent_activity_changed", "session": "ses_main", "handle": "reviewer-3",
            "status": "done", "current_task": null
        }))
        .await;

        assert!(
            h.aux_sessions().is_empty(),
            "terminal lifecycle status should auto-close the observed child pane even when main is focused"
        );
        assert!(
            h.focus_is_main(),
            "auto-closing a background aux pane should leave focus normalized on main"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        let roster = h
            .roster_dialog_state()
            .expect("Subagent manager should open after lifecycle auto-close");
        assert!(
            roster
                .item_sessions
                .iter()
                .any(|session| session == "ses_child"),
            "the roster entry should remain after the observed pane auto-closes"
        );
    }

    #[tokio::test]
    async fn lifecycle_auto_close_member_finished_closes_aux_by_child_session() {
        let mut h = AppHarness::new(120, 30).await;
        open_child_aux(&mut h).await;

        assert_eq!(h.aux_sessions(), vec!["ses_child".to_owned()]);

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        h.push_team_event(json!({
            "type": "member_finished", "status": "done", "child": "ses_child"
        }))
        .await;

        assert!(
            h.aux_sessions().is_empty(),
            "member_finished should close the observed child pane even when the team store does not mutate"
        );
        assert!(
            h.focus_is_main(),
            "auto-closing a member_finished aux pane should leave focus normalized on main"
        );
    }

    #[tokio::test]
    async fn lifecycle_completed_assistant_message_keeps_live_roster_aux_open() {
        let mut h = AppHarness::new(120, 30).await;
        open_child_aux(&mut h).await;

        assert_eq!(h.aux_sessions(), vec!["ses_child".to_owned()]);
        assert!(
            !h.focus_is_main(),
            "the child observation pane starts focused"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        assert!(
            h.focus_is_main(),
            "leader+0 should move focus back to the main pane before the completion event"
        );
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "focusing main should not close the observed child pane"
        );

        h.push_sse(
            "message.updated",
            json!({ "info": {
                "id": "msg_c",
                "sessionID": "ses_child",
                "role": "assistant",
                "time": { "created": 1, "completed": 2 }
            } }),
        )
        .await;

        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "a completed assistant message is only a turn boundary and must not close a live roster observation"
        );
        assert!(
            h.focus_is_main(),
            "ignoring message completion should keep focus on main"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "ignoring message completion should not retarget the main input session"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('o')).await;
        let roster = h
            .roster_dialog_state()
            .expect("Subagent manager should open after message completion");
        assert!(
            roster
                .item_sessions
                .iter()
                .any(|session| session == "ses_child"),
            "the roster entry should remain selectable after the observed child completes one turn"
        );
    }

    #[tokio::test]
    async fn lifecycle_auto_close_incomplete_assistant_message_keeps_aux_open() {
        let mut h = AppHarness::new(120, 30).await;
        open_child_aux(&mut h).await;

        assert_eq!(h.aux_sessions(), vec!["ses_child".to_owned()]);
        assert!(
            !h.focus_is_main(),
            "the child observation pane starts focused"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        assert!(
            h.focus_is_main(),
            "leader+0 should move focus back to the main pane before the message-start update"
        );
        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "focusing main should not close the observed child pane"
        );

        h.push_sse(
            "message.updated",
            json!({ "info": {
                "id": "msg_c2",
                "sessionID": "ses_child",
                "role": "assistant",
                "time": { "created": 2 }
            } }),
        )
        .await;

        assert_eq!(
            h.aux_sessions(),
            vec!["ses_child".to_owned()],
            "an assistant message-start update without completion must not auto-close the observed child pane"
        );
        assert!(
            h.focus_is_main(),
            "ignoring a non-terminal child message update should keep focus on main"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_main"),
            "ignoring a non-terminal child message update should not retarget the main input session"
        );
    }

    #[tokio::test]
    async fn issue20_backend_ready_replays_all_startup_queued_prompts_into_created_session_in_order(
    ) {
        let (transport, client) = recording_client("ses_startup");
        transport.queue_session_create_id("ses_should_stay_unused");
        let mut h = harness_with_client(120, 30, client).await;

        h.type_text("first queued prompt").await;
        h.press(Key::Enter).await;
        h.type_text("second queued prompt").await;
        h.press(Key::Enter).await;

        assert!(
            h.buffer_contains("queued prompt (2)"),
            "precondition: both startup prompts should queue before backend readiness; frame:\n{}",
            h.buffer_text()
        );

        h.backend_ready().await;
        h.settle().await;

        assert_eq!(
            transport.count_method_requests("POST", "/session"),
            1,
            "backend-ready replay should lazily create a single session for the queued startup prompts"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_startup"),
            "replaying queued startup prompts should land on the created session"
        );
        assert_eq!(
            transport.request_bodies("POST", "/session/ses_startup/message"),
            vec![
                json!({ "parts": [{ "type": "text", "text": "first queued prompt" }] }),
                json!({ "parts": [{ "type": "text", "text": "second queued prompt" }] }),
            ],
            "backend-ready replay should post both queued startup prompts to the created session in order"
        );
    }

    #[tokio::test]
    async fn issue20_session_new_clears_startup_queue_and_keeps_lazy_session_creation() {
        let (transport, client) = recording_client("ses_after_new");
        let mut h = harness_with_client(100, 30, client).await;

        h.type_text("old pending").await;
        h.press(Key::Enter).await;
        assert!(
            h.buffer_contains("queued prompt (1)"),
            "startup submit should queue locally before backend readiness; frame:\n{}",
            h.buffer_text()
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('q')).await;
        assert!(
            h.buffer_contains("old pending"),
            "queued-prompts command should show the pending startup prompt; frame:\n{}",
            h.buffer_text()
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('n')).await;
        assert_eq!(
            h.main_route_session(),
            None,
            "/new should navigate to a clean Session screen immediately"
        );
        assert_eq!(h.prompt_text(), "", "/new should clear the prompt composer");

        h.press_ctrl('x').await;
        h.press(Key::Char('q')).await;
        assert!(
            h.buffer_contains("No queued prompts"),
            "/new should clear the startup queue before queued-prompts reporting; frame:\n{}",
            h.buffer_text()
        );

        h.backend_ready().await;
        assert_eq!(
            transport.count_requests("/session"),
            0,
            "/new must not create a persisted empty session before the next submitted prompt"
        );
        assert_eq!(
            transport.count_requests("/session/ses_after_new/message"),
            0,
            "cleared startup prompts must not replay after backend readiness"
        );

        h.type_text("fresh prompt").await;
        h.press(Key::Enter).await;

        assert_eq!(
            transport.count_requests("/session"),
            1,
            "the next submitted prompt after /new should lazily create the persisted session"
        );
        assert_eq!(
            transport.count_requests("/session/ses_after_new/message"),
            1,
            "only the fresh prompt should be sent after /new"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_after_new"),
            "the freshly created session should become the active route"
        );
    }

    #[tokio::test]
    async fn issue20_session_new_clears_submitted_state_and_panes() {
        let (_transport, client) = recording_client("ses_unused");
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;
        open_child_aux(&mut h).await;

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        h.type_text("submitted prompt").await;
        h.press(Key::Enter).await;
        h.press(Key::Up).await;
        assert_eq!(
            h.prompt_text(),
            "submitted prompt",
            "precondition: prompt history should surface the submitted prompt before /new"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('n')).await;
        assert_eq!(
            h.main_route_session(),
            None,
            "/new should navigate away from the old session immediately"
        );
        assert!(
            h.aux_sessions().is_empty(),
            "/new should clear old Session-screen panes"
        );
        assert_eq!(h.prompt_text(), "", "/new should clear the prompt composer");

        h.press(Key::Up).await;
        assert_eq!(
            h.prompt_text(),
            "",
            "/new should reset prompt history so Up does not resurrect the prior submitted prompt"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('q')).await;
        assert!(
            h.buffer_contains("No queued prompts"),
            "submitted prompts should not survive /new as queued prompts; frame:\n{}",
            h.buffer_text()
        );
    }

    #[tokio::test]
    async fn issue20_session_new_schedules_abort_without_blocking_navigation() {
        let (transport, client) = recording_client("ses_unused");
        transport.block_abort();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;
        open_child_aux(&mut h).await;

        h.press_ctrl('x').await;
        h.press(Key::Char('0')).await;
        h.press_ctrl('x').await;
        h.press(Key::Char('n')).await;

        assert_eq!(
            h.main_route_session(),
            None,
            "/new should navigate away from the old session without waiting for abort completion"
        );
        assert_eq!(
            transport.abort_started(),
            1,
            "/new should schedule an asynchronous abort for the old active turn"
        );
        transport.fail_abort("abort backend exploded");

        transport.release_abort();
        h.settle().await;
        assert!(
            h.buffer_contains("abort failed: abort backend exploded"),
            "abort failure should surface as a toast after /new releases the async abort; frame:\n{}",
            h.buffer_text()
        );
    }

    #[tokio::test]
    async fn issue20_session_new_drops_inflight_lazy_first_prompt_submission() {
        let (transport, client) = recording_client("ses_stale");
        transport.block_session_create();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;

        h.type_text("stale prompt").await;
        h.press(Key::Enter).await;
        transport.wait_for_session_create_started(1).await;

        assert_eq!(
            h.main_route_session(),
            None,
            "precondition: the lazy first submit should still be waiting on session creation"
        );

        h.press_ctrl('x').await;
        h.press(Key::Char('n')).await;
        assert_eq!(
            h.main_route_session(),
            None,
            "/new should keep the main route reset while the stale lazy submit is in flight"
        );

        transport.release_session_create();
        h.settle().await;

        assert_eq!(
            h.main_route_session(),
            None,
            "releasing the stale lazy submit after /new must not navigate back into the created session"
        );
        assert_eq!(
            transport.count_requests("/session/ses_stale/message"),
            0,
            "releasing the stale lazy submit after /new must not send the stale prompt"
        );
    }

    #[tokio::test]
    async fn issue20_lazy_first_submit_stale_success_cleans_up_created_session_after_route_change()
    {
        let (transport, client) = recording_client("ses_stale");
        transport.block_session_create();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;

        h.type_text("stale prompt").await;
        h.press(Key::Enter).await;
        transport.wait_for_session_create_started(1).await;

        h.push_sse("session.created", json!({ "info": { "id": "ses_live" } }))
            .await;
        h.dispatch(AppEvent::LoadSession("ses_live".to_owned()))
            .await;
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "precondition: the explicit route change should load the replacement session before the stale create resolves"
        );

        transport.release_session_create();
        h.settle().await;

        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_stale"),
            1,
            "a stale lazy first submit that already created an empty session should clean it up after the route changes"
        );
        assert_eq!(
            transport.count_requests("/session/ses_stale/message"),
            0,
            "the stale lazy first submit must not post its prompt after the main route changes"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "releasing the stale lazy first submit must keep the explicitly loaded session active"
        );
    }
    #[tokio::test]
    async fn issue20_home_lazy_first_submit_accepts_first_create_and_stales_second() {
        let (transport, client) = recording_client("ses_first");
        transport.queue_session_create_id("ses_second");
        transport.block_session_create();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;

        h.type_text("first prompt").await;
        h.press(Key::Enter).await;
        transport.wait_for_session_create_started(1).await;

        h.type_text("second prompt").await;
        h.press(Key::Enter).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            transport.wait_for_session_create_started(2),
        )
        .await
        .expect("submitting a second prompt from Home while the first lazy create is still pending should start a second lazy session create");

        assert_eq!(
            transport.count_method_requests("POST", "/session"),
            2,
            "two Home-route submits before either create resolves should issue two in-flight lazy session creates"
        );
        assert_eq!(
            h.main_route_session(),
            None,
            "precondition: neither lazy create should navigate before the first create returns"
        );

        transport.release_session_create();
        h.settle().await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_first"),
            "accepting the first resolved lazy create should navigate to the first created session"
        );
        assert_eq!(
            transport.request_bodies("POST", "/session/ses_first/message"),
            vec![json!({ "parts": [{ "type": "text", "text": "first prompt" }] })],
            "the first lazy prompt should post exactly once to the first created session"
        );
        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_first"),
            0,
            "accepting the first resolved lazy create must not delete the accepted session"
        );

        transport.release_session_create();
        h.settle().await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_first"),
            "resolving the second stale lazy create must not navigate over the first accepted session"
        );
        assert_eq!(
            transport.count_requests("/session/ses_second/message"),
            0,
            "the second stale lazy create must not post its prompt to the second created session"
        );
        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_second"),
            1,
            "the second stale lazy create should delete only the second newly created empty session"
        );
        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_first"),
            0,
            "the accepted first session must not be deleted when the second create resolves stale"
        );
    }

    #[tokio::test]
    async fn issue20_home_lazy_first_submit_keeps_first_owner_when_second_create_resolves_first() {
        let (transport, client) = recording_client("ses_first");
        transport.queue_session_create_id("ses_second");
        transport.block_session_create();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;

        h.type_text("first prompt").await;
        h.press(Key::Enter).await;
        transport.wait_for_session_create_started(1).await;

        h.type_text("second prompt").await;
        h.press(Key::Enter).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            transport.wait_for_session_create_started(2),
        )
        .await
        .expect("submitting a second prompt from Home while the first lazy create is still pending should start a second lazy session create");

        assert_eq!(
            transport.count_method_requests("POST", "/session"),
            2,
            "two Home-route submits before either create resolves should issue two in-flight lazy session creates"
        );
        assert_eq!(
            h.main_route_session(),
            None,
            "precondition: neither lazy create should navigate before the first create returns"
        );

        transport.release_session_create_ordinal(2);
        h.settle().await;

        assert_eq!(
            h.main_route_session(),
            None,
            "releasing the second lazy create first must not let the later Home submit take over before the first create resolves"
        );
        assert_eq!(
            transport.count_requests("/session/ses_second/message"),
            0,
            "releasing the second lazy create first must not post the second prompt"
        );

        transport.release_session_create_ordinal(1);
        h.settle().await;

        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_first"),
            "once the first lazy create resolves, the first Home submit should own the created session"
        );
        assert_eq!(
            transport.request_bodies("POST", "/session/ses_first/message"),
            vec![json!({ "parts": [{ "type": "text", "text": "first prompt" }] })],
            "the first lazy prompt should post exactly once to the first created session even when the second create response wins the race"
        );
        assert_eq!(
            transport.count_requests("/session/ses_second/message"),
            0,
            "the second lazy create must never post its prompt when it resolves before the first create"
        );
        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_second"),
            1,
            "the second created session should be deleted as stale after the first Home submit wins ownership"
        );
        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_first"),
            0,
            "the accepted first session must not be deleted when the second create response resolves earlier"
        );
        assert!(
            !h.buffer_contains("second prompt"),
            "the stale second prompt must not render in the owned session after cleanup settles; frame:\n{}",
            h.buffer_text()
        );

        h.press(Key::Up).await;
        let recalled = h.prompt_text().to_string();
        assert!(
            recalled.is_empty() || recalled == "first prompt",
            "prompt history after stale cleanup must not resurrect the stale second prompt; got {recalled:?}"
        );
    }

    #[tokio::test]
    async fn issue20_lazy_first_submit_stale_create_error_does_not_render_after_route_change() {
        let (transport, client) = recording_client("ses_stale");
        transport.block_session_create();
        transport.fail_session_create("session backend exploded");
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;

        h.type_text("stale prompt").await;
        h.press(Key::Enter).await;
        transport.wait_for_session_create_started(1).await;

        h.push_sse("session.created", json!({ "info": { "id": "ses_live" } }))
            .await;
        h.dispatch(AppEvent::LoadSession("ses_live".to_owned()))
            .await;
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "precondition: the explicit route change should load the replacement session before the stale create error resolves"
        );

        transport.release_session_create();
        h.settle().await;

        assert!(
            !h.buffer_contains("session create failed"),
            "a stale session-create failure must not render a toast after the main route changes; frame:\n{}",
            h.buffer_text()
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "the stale create failure must keep the explicitly loaded session active"
        );
    }

    #[tokio::test]
    async fn issue20_command_palette_lazy_create_stale_route_change_cleans_up_without_posting_command(
    ) {
        let (transport, client) = recording_client("ses_stale");
        transport.block_session_create();
        let mut h = harness_with_client(120, 30, client).await;
        h.backend_ready().await;
        h.dispatch(AppEvent::CommandList(vec!["review".to_owned()]))
            .await;

        assert_eq!(
            h.main_route_session(),
            None,
            "precondition: the command-palette action should start from a non-session route"
        );

        h.press_ctrl('p').await;
        assert!(
            h.buffer_contains("Commands"),
            "ctrl+p should open the command palette; frame:\n{}",
            h.buffer_text()
        );

        h.type_text("review").await;
        let frame = h.buffer_text();
        let (row, column) = frame
            .lines()
            .enumerate()
            .filter_map(|(row, line)| {
                line.find("review")
                    .map(|column| (row as u16, column as u16))
            })
            .last()
            .expect("filtering the command palette should render the discovered review command");
        h.dispatch(AppEvent::Mouse {
            column,
            row,
            kind: super::super::MouseKind::Press,
        })
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            transport.wait_for_session_create_started(1),
        )
        .await
        .expect("selecting the discovered review command from the command palette should start lazy session creation");

        assert_eq!(
            h.main_route_session(),
            None,
            "precondition: the command-palette lazy create should still be waiting on session creation"
        );

        h.push_sse("session.created", json!({ "info": { "id": "ses_live" } }))
            .await;
        h.dispatch(AppEvent::LoadSession("ses_live".to_owned()))
            .await;
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "precondition: the explicit route change should load the replacement session before the stale command create resolves"
        );

        transport.release_session_create();
        h.settle().await;

        assert_eq!(
            transport.count_method_requests("DELETE", "/session/ses_stale"),
            1,
            "a stale command-palette lazy create should delete the empty session it created after the route changes"
        );
        assert_eq!(
            transport.count_requests("/session/ses_stale/command"),
            0,
            "the stale command-palette lazy create must not post its command after the main route changes"
        );
        assert_eq!(
            h.main_route_session().as_deref(),
            Some("ses_live"),
            "releasing the stale command-palette lazy create must keep the explicitly loaded session active"
        );
    }

    #[tokio::test]
    async fn child_route_channels_overlay_uses_root_team_scope_for_inbox_mail() {
        let mut h = AppHarness::new(120, 30).await;
        h.push_sse("session.created", json!({ "info": { "id": "ses_main" } }))
            .await;
        h.push_sse(
            "session.created",
            json!({ "info": { "id": "ses_child", "parentID": "ses_main" } }),
        )
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_main",
            "handle": "main", "agent_type": "main", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "agent_registered", "session": "ses_main", "agent_session": "ses_child",
            "handle": "reviewer-1", "agent_type": "reviewer", "mode": "resident"
        }))
        .await;
        h.push_team_event(json!({
            "type": "mail_sent", "session": "ses_main", "from": "main",
            "to": { "kind": "handle", "id": "reviewer-1" }, "body": "please review"
        }))
        .await;

        h.navigate("ses_child").await;
        h.press_ctrl('x').await;
        h.press(Key::Char('i')).await;

        assert!(
            h.buffer_contains("Channels & inboxes"),
            "leader+i opens the channel/inbox overlay from a child route; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            h.buffer_contains("main: please review"),
            "a child route should resolve the root Team projection and show root-scoped inbox mail; frame:\n{}",
            h.buffer_text()
        );
        assert!(
            !h.buffer_contains("No channel activity yet"),
            "root-scoped inbox mail should replace the empty placeholder on a child route; frame:\n{}",
            h.buffer_text()
        );
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
