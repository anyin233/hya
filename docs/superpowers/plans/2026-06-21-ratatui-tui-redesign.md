# Ratatui TUI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a polished opencode-inspired, Rust-only ratatui TUI for hya's existing chat/session state.

**Architecture:** Keep `hya-cli` as the terminal/event-loop owner and keep `hya-render-tui` as a pure view crate. Split rendering into semantic theme, responsive layout, projection view-model, and focused widgets while preserving `AppState` and `draw`.

**Tech Stack:** Rust 2024, `ratatui 0.28`, `crossterm 0.28` in CLI, `ratatui::backend::TestBackend` for render tests.

---

## File Structure

- Create: `crates/hya-render-tui/src/theme.rs` — semantic colors and style helpers.
- Create: `crates/hya-render-tui/src/layout.rs` — status/body/sidebar/prompt/footer rectangle calculation.
- Create: `crates/hya-render-tui/src/view_model.rs` — convert `Projection` messages and parts into renderable timeline items.
- Create: `crates/hya-render-tui/src/widgets.rs` — draw status, timeline, prompt, sidebar, and footer.
- Modify: `crates/hya-render-tui/src/lib.rs` — keep public state and delegate drawing to new modules.
- Modify: `crates/hya-render-tui/tests/tui_render.rs` — add failing tests first, then update old expectations.

### Task 1: Failing Render Tests

**Files:**
- Modify: `crates/hya-render-tui/tests/tui_render.rs`

- [ ] **Step 1: Add tests for desired layout behavior**

```rust
#[test]
fn wide_layout_renders_sidebar_and_surface_labels() {
    let mut state = rich_state();
    let text = render(&mut state, 120, 36);
    assert!(text.contains("context"), "wide layout should show context sidebar");
    assert!(text.contains("model fake"), "sidebar should show model");
    assert!(text.contains("session sess-1"), "sidebar should show session label");
    assert!(text.contains("team"), "sidebar should summarize team");
}

#[test]
fn narrow_layout_hides_sidebar_without_hiding_prompt() {
    let mut state = rich_state();
    let text = render(&mut state, 80, 24);
    assert!(!text.contains("context"), "narrow layout should hide sidebar");
    assert!(text.contains("type here"), "prompt must remain visible");
    assert!(text.contains("HELLOTUI"), "transcript must remain visible");
}

#[test]
fn timeline_renders_message_rails_and_tool_status() {
    let mut state = AppState::default();
    state.model = "fake".to_string();
    state.session_label = "sess-1".to_string();
    with_user_message(&mut state, "please inspect files");
    with_tool_message(&mut state);
    let text = render(&mut state, 120, 30);
    assert!(text.contains("You"), "user label should render");
    assert!(text.contains("│"), "timeline should use a left rail");
    assert!(text.contains("tool read completed"), "completed tool should render as a compact status row");
}
```

- [ ] **Step 2: Run test to verify RED**

Run: `cargo test -p hya-render-tui wide_layout_renders_sidebar_and_surface_labels narrow_layout_hides_sidebar_without_hiding_prompt timeline_renders_message_rails_and_tool_status`

Expected: FAIL because the current renderer has no `context` sidebar, no `model fake` sidebar label, and no compact `tool read completed` row.

### Task 2: Theme and Layout

**Files:**
- Create: `crates/hya-render-tui/src/theme.rs`
- Create: `crates/hya-render-tui/src/layout.rs`

- [ ] **Step 1: Implement semantic theme**

```rust
pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub element: Color,
    pub border_subtle: Color,
    pub border_active: Color,
    pub text: Color,
    pub muted: Color,
    pub primary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
}

impl Theme {
    pub const fn hya_dark() -> Self {
        Self {
            background: Color::Rgb(10, 10, 10),
            panel: Color::Rgb(20, 20, 20),
            element: Color::Rgb(30, 30, 30),
            border_subtle: Color::Rgb(60, 60, 60),
            border_active: Color::Rgb(96, 96, 96),
            text: Color::Rgb(238, 238, 238),
            muted: Color::Rgb(128, 128, 128),
            primary: Color::Rgb(250, 178, 131),
            accent: Color::Rgb(157, 124, 216),
            success: Color::Rgb(127, 216, 143),
            warning: Color::Rgb(245, 167, 66),
            error: Color::Rgb(224, 108, 117),
            info: Color::Rgb(86, 182, 194),
        }
    }
}
```

- [ ] **Step 2: Implement responsive layout**

```rust
pub struct AppLayout {
    pub status: Rect,
    pub timeline: Rect,
    pub sidebar: Option<Rect>,
    pub prompt: Rect,
    pub footer: Rect,
}

pub fn app_layout(area: Rect) -> AppLayout {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let show_sidebar = rows[1].width >= 110;
    let body = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(38)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(rows[1])
    };
    AppLayout {
        status: rows[0],
        timeline: body[0],
        sidebar: show_sidebar.then_some(body[1]),
        prompt: rows[2],
        footer: rows[3],
    }
}
```

- [ ] **Step 3: Run focused tests**

Run: `cargo test -p hya-render-tui wide_layout_renders_sidebar_and_surface_labels`

Expected: Still FAIL until widgets and `draw` use the layout.

### Task 3: View Model and Widgets

**Files:**
- Create: `crates/hya-render-tui/src/view_model.rs`
- Create: `crates/hya-render-tui/src/widgets.rs`
- Modify: `crates/hya-render-tui/src/lib.rs`

- [ ] **Step 1: Implement timeline conversion**

```rust
pub enum TimelinePart {
    Text(String),
    Reasoning(String),
    Tool { name: String, status: ToolStatus },
}

pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Error,
}

pub struct TimelineItem {
    pub role: Role,
    pub parts: Vec<TimelinePart>,
}

pub fn timeline_items(projection: &Projection) -> Vec<TimelineItem> {
    projection
        .session
        .messages
        .iter()
        .map(|message| TimelineItem {
            role: message.role,
            parts: message.parts.iter().map(part_to_timeline).collect(),
        })
        .collect()
}
```

- [ ] **Step 2: Implement widget rendering helpers**

```rust
pub fn render_status(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(Paragraph::new(status_line(state, theme)), area);
}

pub fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let lines = sidebar_lines(state, theme);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" context ")
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(theme.border_subtle)),
        ),
        area,
    );
}
```

- [ ] **Step 3: Refactor `draw` to delegate**

```rust
pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let theme = Theme::hya_dark();
    let layout = app_layout(frame.area());
    render_status(frame, layout.status, state, &theme);
    render_timeline(frame, layout.timeline, state, &theme);
    if let Some(sidebar) = layout.sidebar {
        render_sidebar(frame, sidebar, state, &theme);
    }
    render_prompt(frame, layout.prompt, state, &theme);
    render_footer(frame, layout.footer, state, &theme);
}
```

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p hya-render-tui`

Expected: PASS for new and existing TUI render tests.

### Task 4: Full Verification

**Files:**
- Modify only if formatting or tests reveal issues.

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`

Expected: exit 0.

- [ ] **Step 2: Clippy check**

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: exit 0.

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace`

Expected: exit 0.
