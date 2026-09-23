//! JSON string width accounting for shell output artifacts.

pub(crate) const MAX_CODING_OUTPUT_PATH_BYTES: usize = 32 * 1024;

pub(crate) fn json_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{08}' | '\u{0c}' | '\n' | '\r' | '\t' => 2,
        character if character < ' ' => 6,
        character => character.len_utf8(),
    }
}

pub(crate) fn serialized_string_len(text: &str) -> usize {
    text.chars().fold(2usize, |used, character| {
        used.saturating_add(json_char_len(character))
    })
}
