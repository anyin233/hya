pub(crate) fn strip(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                let _ = chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let _ = chars.next();
                consume_string_escape(&mut chars);
            }
            Some('P' | '^' | '_' | 'X') => {
                let _ = chars.next();
                consume_string_escape(&mut chars);
            }
            Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                let _ = chars.next();
                let _ = chars.next();
            }
            Some(_) => {
                let _ = chars.next();
            }
            None => {}
        }
    }
    output
}

pub(crate) fn clean_inline(text: &str) -> Option<String> {
    let stripped = strip(text);
    let cleaned = stripped
        .chars()
        .filter_map(inline_char)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

pub(crate) fn clean_multiline(text: &str) -> Option<String> {
    let stripped = strip(text);
    let cleaned = stripped
        .trim_matches(|ch| matches!(ch, '\n' | '\r'))
        .chars()
        .filter_map(multiline_char)
        .collect::<String>();
    (!cleaned.trim().is_empty()).then_some(cleaned)
}

fn inline_char(ch: char) -> Option<char> {
    match ch {
        '\n' | '\t' | '\r' => Some(' '),
        ch if ch.is_control() => None,
        ch => Some(ch),
    }
}

fn multiline_char(ch: char) -> Option<char> {
    match ch {
        '\n' => Some('\n'),
        '\t' => Some(' '),
        '\r' => None,
        ch if ch.is_control() => None,
        ch => Some(ch),
    }
}

fn consume_string_escape<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(next) = chars.next() {
        if next == '\u{7}' {
            break;
        }
        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
            let _ = chars.next();
            break;
        }
    }
}
