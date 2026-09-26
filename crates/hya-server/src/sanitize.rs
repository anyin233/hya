//! Text from a remote party (a relay proxy's error, a close reason) made
//! safe to print on a terminal or store for display: terminal escape
//! sequences and control characters are removed.

/// `text` without escape sequences (CSI `ESC [ … final`, OSC `ESC ] … BEL`
/// or `ESC \`, other `ESC x` pairs), C0 controls other than `\n` and `\t`,
/// DEL, and C1 controls (U+0080–U+009F).
#[must_use]
pub fn display_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates up to a final byte.
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: a string up to BEL or ST.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' || next == '\u{9c}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other escape: the one character after ESC.
                _ => {}
            },
            '\n' | '\t' => out.push(ch),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::display_text;

    #[test]
    fn keeps_plain_text_newlines_and_tabs() {
        assert_eq!(
            display_text("relay down: 503\tretry\nnext"),
            "relay down: 503\tretry\nnext"
        );
        assert_eq!(display_text("héllo — ok"), "héllo — ok");
    }

    #[test]
    fn strips_escape_sequences() {
        assert_eq!(display_text("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(display_text("a\u{1b}]0;pwned\u{7}b"), "ab");
        assert_eq!(
            display_text("\u{1b}]8;;https://evil.example\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(display_text("x\u{1b}Py"), "x");
        assert_eq!(display_text("x\u{1b}cy"), "xy");
        assert_eq!(display_text("trailing\u{1b}"), "trailing");
    }

    #[test]
    fn strips_c0_del_and_c1_controls() {
        assert_eq!(display_text("a\u{0}b\u{7}c\rd\u{8}e\u{7f}f"), "abcdef");
        assert_eq!(display_text("a\u{9b}31mb\u{85}c"), "a31mbc");
    }
}
