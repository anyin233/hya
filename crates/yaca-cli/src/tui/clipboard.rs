use std::io::{self, Write};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

#[must_use]
pub(super) fn osc52_sequence(text: &str) -> String {
    let encoded = STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

pub(super) fn write_clipboard(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(osc52_sequence(text).as_bytes())?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_sequence_encodes_clipboard_text() {
        assert_eq!(osc52_sequence("copy"), "\x1b]52;c;Y29weQ==\x07");
    }
}
