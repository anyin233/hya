//! Line prompts on stderr answered on stdin, for interactive CLI commands.
//! They work the same on a terminal and on a pipe (tests, scripts): a closed
//! stdin answers `None`.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

use anyhow::Context as _;

/// Ask `question` and read one trimmed answer line; `None` when stdin is
/// closed. With `hidden`, a terminal does not echo the answer.
pub(crate) fn ask(question: &str, hidden: bool) -> anyhow::Result<Option<String>> {
    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    write!(stderr, "{question}").context("write prompt")?;
    stderr.flush().context("flush prompt")?;
    let mut line = String::new();
    let read = if hidden && stdin.is_terminal() {
        let echo = EchoOff::new();
        let read = stdin.lock().read_line(&mut line);
        drop(echo);
        eprintln!();
        read
    } else {
        stdin.lock().read_line(&mut line)
    }
    .context("read answer from stdin")?;
    if read == 0 {
        eprintln!();
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

/// Ask a yes/no `question`; only `y`/`yes` (any case) is yes, and a closed
/// stdin is no.
pub(crate) fn confirm(question: &str) -> anyhow::Result<bool> {
    Ok(ask(&format!("{question} [y/N] "), false)?
        .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")))
}

/// Terminal echo off for its lifetime (best effort).
pub(crate) struct EchoOff(Option<libc::termios>);

impl EchoOff {
    pub(crate) fn new() -> Self {
        // SAFETY: `termios` is plain data; `tcgetattr`/`tcsetattr` only read
        // and write it for fd 0, which is open (it is our stdin).
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return Self(None);
            }
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) != 0 {
                return Self(None);
            }
            Self(Some(saved))
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(saved) = self.0 {
            // SAFETY: restores the attributes read in `new` on the same fd.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
    }
}
