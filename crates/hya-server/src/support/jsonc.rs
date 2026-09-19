use serde::de::DeserializeOwned;

pub(super) fn from_str<T: DeserializeOwned>(input: &str) -> serde_json::Result<T> {
    let without_comments = strip_comments(input);
    let normalized = strip_trailing_commas(&without_comments);
    serde_json::from_str(&normalized)
}

fn strip_comments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if ch == '/' && chars.peek().is_some_and(|next| *next == '/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if ch == '/' && chars.peek().is_some_and(|next| *next == '*') {
            chars.next();
            strip_block_comment(&mut chars, &mut output);
        } else {
            output.push(ch);
        }
    }
    output
}

fn strip_block_comment(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, output: &mut String) {
    let mut previous = '\0';
    for next in chars.by_ref() {
        if next == '\n' {
            output.push('\n');
        }
        if previous == '*' && next == '/' {
            break;
        }
        previous = next;
    }
}

fn strip_trailing_commas(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if ch == ','
            && next_non_whitespace(chars.clone()).is_some_and(|next| next == '}' || next == ']')
        {
            continue;
        } else {
            output.push(ch);
        }
    }
    output
}

fn next_non_whitespace(mut chars: std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    chars.find(|ch| !ch.is_whitespace())
}
