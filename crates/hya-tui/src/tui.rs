use std::io::{self, Stdout};
use std::panic;

use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
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
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{AppEvent, MouseKind};
use crate::contracts::{Key, KeyEvent};

pub type Backend = CrosstermBackend<Stdout>;

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
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<Backend> {
        &mut self.terminal
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        restore_terminal(self.terminal.backend_mut())?;
        self.terminal.show_cursor()?;
        self.restored = true;
        Ok(())
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        )?;
        disable_raw_mode()?;
        self.terminal.show_cursor()
    }

    pub fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
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
