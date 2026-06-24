use crate::contracts::{KeyChord, KeyEvent};

use super::atom::{parse_key_atom, KeyAtom};
use super::util::starts_with_ignore_ascii_case;
use super::ParseKeyBindingError;

pub(crate) fn parse_binding_spec_inner(
    input: &str,
    leader: Option<&KeyChord>,
) -> Result<Vec<KeyChord>, ParseKeyBindingError> {
    if input.trim().is_empty() {
        return Err(ParseKeyBindingError::EmptySpec);
    }
    input
        .split(',')
        .enumerate()
        .map(|(index, alternative)| parse_alternative(alternative, leader, index))
        .collect()
}

fn parse_alternative(
    input: &str,
    leader: Option<&KeyChord>,
    index: usize,
) -> Result<KeyChord, ParseKeyBindingError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ParseKeyBindingError::EmptyAlternative { index });
    }
    let mut events = Vec::new();
    let mut cursor = 0usize;
    while cursor < input.len() {
        cursor = skip_whitespace(input, cursor);
        if cursor >= input.len() {
            break;
        }
        let rest = &input[cursor..];
        if starts_with_ignore_ascii_case(rest, "<leader>") {
            let leader = leader.ok_or(ParseKeyBindingError::LeaderUnavailable)?;
            events.extend(leader.0.iter().copied());
            cursor += "<leader>".len();
            continue;
        }
        let parsed = parse_key_event(input, cursor)?;
        events.push(parsed.event);
        cursor = parsed.next;
    }
    Ok(KeyChord(events))
}

fn parse_key_event(input: &str, start: usize) -> Result<ParsedEvent, ParseKeyBindingError> {
    let mut cursor = start;
    let mut modifiers = KeyModifiers::default();
    while let Some(modifier) = consume_modifier(&input[cursor..]) {
        modifier.kind.apply(&mut modifiers);
        cursor += modifier.consumed;
    }
    if input[cursor..].starts_with('+') {
        return Err(ParseKeyBindingError::UnknownKey {
            key: "+".to_owned(),
        });
    }
    let atom = parse_key_atom(&input[cursor..])?;
    Ok(ParsedEvent {
        event: modifiers.apply(atom),
        next: cursor + atom.consumed,
    })
}

fn consume_modifier(input: &str) -> Option<ParsedModifier> {
    const MODIFIERS: &[(&str, ModifierKind)] = &[
        ("control+", ModifierKind::Ctrl),
        ("ctrl+", ModifierKind::Ctrl),
        ("shift+", ModifierKind::Shift),
        ("meta+", ModifierKind::Meta),
        ("super+", ModifierKind::Meta),
        ("hyper+", ModifierKind::Meta),
        ("alt+", ModifierKind::Alt),
    ];
    MODIFIERS.iter().find_map(|(name, kind)| {
        starts_with_ignore_ascii_case(input, name).then_some(ParsedModifier {
            kind: *kind,
            consumed: name.len(),
        })
    })
}

fn skip_whitespace(input: &str, start: usize) -> usize {
    let mut cursor = start;
    while let Some(ch) = input[cursor..].chars().next() {
        if !ch.is_whitespace() {
            break;
        }
        cursor += ch.len_utf8();
        if cursor >= input.len() {
            break;
        }
    }
    cursor
}

#[derive(Debug, Clone, Copy, Default)]
struct KeyModifiers {
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
}

impl KeyModifiers {
    fn apply(self, atom: KeyAtom) -> KeyEvent {
        KeyEvent {
            key: atom.key,
            ctrl: self.ctrl,
            alt: self.alt,
            shift: self.shift || atom.shift,
            meta: self.meta,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedEvent {
    event: KeyEvent,
    next: usize,
}

#[derive(Debug, Clone, Copy)]
struct ParsedModifier {
    kind: ModifierKind,
    consumed: usize,
}

#[derive(Debug, Clone, Copy)]
enum ModifierKind {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

impl ModifierKind {
    fn apply(self, modifiers: &mut KeyModifiers) {
        match self {
            Self::Ctrl => modifiers.ctrl = true,
            Self::Alt => modifiers.alt = true,
            Self::Shift => modifiers.shift = true,
            Self::Meta => modifiers.meta = true,
        }
    }
}
