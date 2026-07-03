use std::io::{self, Stdout};
use std::panic;

use futures_util::StreamExt;
use ratatui::backend::{ClearType, CrosstermBackend, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use ratatui::crossterm::event::{
    Event as CrosstermEvent, EventStream, KeyCode, KeyEvent as CrosstermKeyEvent, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Position, Size};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{AppEvent, MouseKind};
use crate::contracts::{Key, KeyEvent};

/// Terminal backend that is either the real crossterm terminal (production) or an in-memory
/// [`TestBackend`] (headless tests).
///
/// Kept as a single concrete type on purpose: `Tui`, `Runtime`, and `run_tui` stay non-generic,
/// so enabling headless tests costs no churn in the render loop. A `Box<dyn Backend>` is not an
/// option because `ratatui::backend::Backend` has generic methods (`draw`, `set_cursor_position`)
/// and so is not object-safe.
pub enum AnyBackend {
    Crossterm(CrosstermBackend<Stdout>),
    Test(TestBackend),
}

impl ratatui::backend::Backend for AnyBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        match self {
            Self::Crossterm(backend) => backend.draw(content),
            Self::Test(backend) => backend.draw(content),
        }
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.hide_cursor(),
            Self::Test(backend) => backend.hide_cursor(),
        }
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.show_cursor(),
            Self::Test(backend) => backend.show_cursor(),
        }
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        match self {
            Self::Crossterm(backend) => backend.get_cursor_position(),
            Self::Test(backend) => backend.get_cursor_position(),
        }
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.set_cursor_position(position),
            Self::Test(backend) => backend.set_cursor_position(position),
        }
    }

    fn clear(&mut self) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.clear(),
            Self::Test(backend) => backend.clear(),
        }
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.clear_region(clear_type),
            Self::Test(backend) => backend.clear_region(clear_type),
        }
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.append_lines(n),
            Self::Test(backend) => backend.append_lines(n),
        }
    }

    fn size(&self) -> io::Result<Size> {
        match self {
            Self::Crossterm(backend) => backend.size(),
            Self::Test(backend) => backend.size(),
        }
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        match self {
            Self::Crossterm(backend) => backend.window_size(),
            Self::Test(backend) => backend.window_size(),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Crossterm(backend) => backend.flush(),
            Self::Test(backend) => backend.flush(),
        }
    }
}

pub type Backend = AnyBackend;

pub struct Tui {
    terminal: Terminal<Backend>,
    restored: bool,
}

impl Tui {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let mut terminal = Terminal::new(AnyBackend::Crossterm(CrosstermBackend::new(stdout)))?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    /// Build a headless `Tui` backed by an in-memory [`TestBackend`] for tests. No raw mode,
    /// no alternate screen, no stdout writes — the rendered frame is inspectable via
    /// [`Tui::test_buffer`].
    pub fn from_test_backend(width: u16, height: u16) -> io::Result<Self> {
        let terminal = Terminal::new(AnyBackend::Test(TestBackend::new(width, height)))?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    /// The rendered cell buffer when this `Tui` is backed by a [`TestBackend`]; `None` for the
    /// real crossterm terminal.
    pub fn test_buffer(&self) -> Option<&Buffer> {
        match self.terminal.backend() {
            AnyBackend::Test(backend) => Some(backend.buffer()),
            AnyBackend::Crossterm(_) => None,
        }
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<Backend> {
        &mut self.terminal
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        if let AnyBackend::Crossterm(backend) = self.terminal.backend_mut() {
            restore_terminal(backend)?;
        }
        self.terminal.show_cursor()?;
        self.restored = true;
        Ok(())
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        if let AnyBackend::Crossterm(backend) = self.terminal.backend_mut() {
            execute!(
                backend,
                LeaveAlternateScreen,
                DisableMouseCapture,
                DisableBracketedPaste
            )?;
            disable_raw_mode()?;
        }
        self.terminal.show_cursor()
    }

    pub fn resume(&mut self) -> io::Result<()> {
        if let AnyBackend::Crossterm(backend) = self.terminal.backend_mut() {
            enable_raw_mode()?;
            execute!(
                backend,
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste
            )?;
        }
        self.terminal.clear()?;
        self.restored = false;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn install_panic_hook() {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let mut stdout = io::stdout();
        let _ = restore_terminal(&mut stdout);
        default(info);
    }));
}

pub fn spawn_input_task(tx: mpsc::UnboundedSender<AppEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(event) = events.next().await {
            match event {
                Ok(event) => {
                    if let Some(app_event) = map_event(event) {
                        if tx.send(app_event).is_err() {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(AppEvent::Internal(format!("terminal input error: {error}")));
                    break;
                }
            }
        }
    })
}

#[must_use]
pub fn map_event(event: CrosstermEvent) -> Option<AppEvent> {
    match event {
        CrosstermEvent::Key(event) if event.kind == KeyEventKind::Press => {
            Some(AppEvent::Key(map_key_event(event)))
        }
        CrosstermEvent::Key(_) => None,
        CrosstermEvent::Paste(text) => Some(AppEvent::Paste(text)),
        CrosstermEvent::Resize(width, height) => Some(AppEvent::Resize(width, height)),
        CrosstermEvent::Mouse(mouse) => {
            let kind = match mouse.kind {
                MouseEventKind::ScrollUp => MouseKind::ScrollUp,
                MouseEventKind::ScrollDown => MouseKind::ScrollDown,
                MouseEventKind::Down(_) => MouseKind::Press,
                _ => MouseKind::Other,
            };
            Some(AppEvent::Mouse {
                column: mouse.column,
                row: mouse.row,
                kind,
            })
        }
        CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => None,
    }
}

#[must_use]
pub fn map_key_event(event: CrosstermKeyEvent) -> KeyEvent {
    KeyEvent {
        key: map_key(event.code),
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
        meta: event.modifiers.contains(KeyModifiers::SUPER)
            || event.modifiers.contains(KeyModifiers::META),
    }
}

fn map_key(code: KeyCode) -> Key {
    match code {
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::F(value) => Key::F(value),
        KeyCode::Char(ch) => Key::Char(ch),
        KeyCode::Esc => Key::Esc,
        KeyCode::Null | KeyCode::CapsLock | KeyCode::ScrollLock | KeyCode::NumLock => Key::Esc,
        KeyCode::PrintScreen | KeyCode::Pause | KeyCode::Menu | KeyCode::KeypadBegin => Key::Esc,
        KeyCode::Media(_) | KeyCode::Modifier(_) => Key::Esc,
    }
}

fn restore_terminal<W: io::Write>(writer: &mut W) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        writer,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )
}
