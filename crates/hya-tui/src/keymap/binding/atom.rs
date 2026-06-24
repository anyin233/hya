use crate::contracts::Key;

use super::util::starts_with_ignore_ascii_case;
use super::ParseKeyBindingError;

#[derive(Debug, Clone, Copy)]
pub(super) struct KeyAtom {
    pub key: Key,
    pub consumed: usize,
    pub shift: bool,
}

pub(super) fn parse_key_atom(input: &str) -> Result<KeyAtom, ParseKeyBindingError> {
    if input.starts_with('<') {
        return parse_angle_key(input);
    }
    if let Some(atom) = parse_named_key(input) {
        return Ok(atom);
    }
    parse_char_key(input)
}

fn parse_angle_key(input: &str) -> Result<KeyAtom, ParseKeyBindingError> {
    let Some(close) = input.find('>') else {
        return Err(ParseKeyBindingError::UnterminatedToken {
            token: input.to_owned(),
        });
    };
    let name = &input[1..close];
    named_key(name).map_or_else(
        || {
            Err(ParseKeyBindingError::UnknownKey {
                key: name.to_owned(),
            })
        },
        |key| {
            Ok(KeyAtom {
                key,
                consumed: close + 1,
                shift: false,
            })
        },
    )
}

fn parse_named_key(input: &str) -> Option<KeyAtom> {
    const NAMES: &[&str] = &[
        "backspace",
        "pagedown",
        "pageup",
        "return",
        "escape",
        "backtab",
        "delete",
        "insert",
        "pgdown",
        "space",
        "enter",
        "right",
        "left",
        "down",
        "home",
        "pgdn",
        "pgup",
        "tab",
        "end",
        "esc",
        "del",
        "ins",
        "up",
    ];
    parse_function_key(input).or_else(|| {
        NAMES.iter().find_map(|name| {
            let key = named_key(name)?;
            starts_with_ignore_ascii_case(input, name).then_some(KeyAtom {
                key,
                consumed: name.len(),
                shift: false,
            })
        })
    })
}

fn parse_function_key(input: &str) -> Option<KeyAtom> {
    let rest = input
        .strip_prefix('f')
        .or_else(|| input.strip_prefix('F'))?;
    let digit_len = rest.chars().take_while(char::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let name = &rest[..digit_len];
    let value = name.parse::<u8>().ok()?;
    Some(KeyAtom {
        key: Key::F(value),
        consumed: 1 + name.len(),
        shift: false,
    })
}

fn parse_char_key(input: &str) -> Result<KeyAtom, ParseKeyBindingError> {
    let Some(ch) = input.chars().next() else {
        return Err(ParseKeyBindingError::EmptySpec);
    };
    Ok(KeyAtom {
        key: Key::Char(ch.to_ascii_lowercase()),
        consumed: ch.len_utf8(),
        shift: ch.is_ascii_uppercase(),
    })
}

fn named_key(name: &str) -> Option<Key> {
    match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => Some(Key::Enter),
        "escape" | "esc" => Some(Key::Esc),
        "backspace" => Some(Key::Backspace),
        "tab" => Some(Key::Tab),
        "backtab" => Some(Key::BackTab),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "pgup" => Some(Key::PageUp),
        "pagedown" | "pgdown" | "pgdn" => Some(Key::PageDown),
        "delete" | "del" => Some(Key::Delete),
        "insert" | "ins" => Some(Key::Insert),
        "space" => Some(Key::Char(' ')),
        _ => None,
    }
}
