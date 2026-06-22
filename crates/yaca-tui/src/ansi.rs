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
