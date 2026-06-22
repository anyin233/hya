use yaca_tui::{AppState, PromptAttachment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PasteOutcome {
    pub refresh_popup: bool,
}

#[derive(Default)]
pub(super) struct PromptState {
    paste_entries: Vec<PasteEntry>,
    last_paste_pending_reveal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteEntry {
    placeholder: String,
    original: String,
}

impl PromptState {
    pub fn handle_paste(&mut self, app: &mut AppState, text: &str) -> PasteOutcome {
        app.exit_armed = false;
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.last_paste_pending_reveal {
            self.reveal_last_paste(&mut app.input);
        }
        let pasted = normalized.trim();
        if pasted.is_empty() {
            return PasteOutcome {
                refresh_popup: false,
            };
        }
        if let Some((path, mime)) = image_paste(pasted) {
            let placeholder = format!("[Image #{}]", app.attachments.len() + 1);
            app.attachments.push(PromptAttachment {
                placeholder: placeholder.clone(),
                source_path: Some(path.clone()),
                mime: mime.to_string(),
            });
            app.input.push_str(&placeholder);
            app.input.push(' ');
            app.input_cursor = None;
            self.paste_entries.push(PasteEntry {
                placeholder,
                original: path,
            });
            self.last_paste_pending_reveal = true;
            return PasteOutcome {
                refresh_popup: false,
            };
        }
        let line_count = pasted.matches('\n').count() + 1;
        if line_count >= 3 || pasted.chars().count() > 150 {
            let placeholder = format!("[Pasted Text #{}]", self.paste_entries.len() + 1);
            app.input.push_str(&placeholder);
            app.input.push(' ');
            app.input_cursor = None;
            self.paste_entries.push(PasteEntry {
                placeholder,
                original: normalized,
            });
            self.last_paste_pending_reveal = true;
            return PasteOutcome {
                refresh_popup: false,
            };
        }
        app.input.push_str(&normalized);
        app.input_cursor = None;
        self.last_paste_pending_reveal = false;
        PasteOutcome {
            refresh_popup: true,
        }
    }

    pub fn clear(&mut self, app: &mut AppState) {
        app.input.clear();
        app.input_cursor = None;
        app.attachments.clear();
        self.paste_entries.clear();
        self.last_paste_pending_reveal = false;
    }

    pub fn insert_char(&mut self, app: &mut AppState, ch: char) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        app.input.insert(cursor, ch);
        app.input_cursor = Some(cursor + ch.len_utf8());
        self.last_paste_pending_reveal = false;
    }

    pub fn backspace(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        let previous = previous_boundary(&app.input, cursor);
        if previous == cursor {
            app.input_cursor = Some(cursor);
            return;
        }
        app.input.replace_range(previous..cursor, "");
        app.input_cursor = Some(previous);
        self.last_paste_pending_reveal = false;
    }

    pub fn delete(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        let next = next_boundary(&app.input, cursor);
        if next == cursor {
            app.input_cursor = Some(cursor);
            return;
        }
        app.input.replace_range(cursor..next, "");
        app.input_cursor = Some(cursor);
        self.last_paste_pending_reveal = false;
    }

    pub fn move_cursor_left(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        app.input_cursor = Some(previous_boundary(&app.input, cursor));
    }

    pub fn move_cursor_right(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        app.input_cursor = Some(next_boundary(&app.input, cursor));
    }

    pub fn move_cursor_line_start(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        app.input_cursor = Some(line_start(&app.input, cursor));
    }

    pub fn move_cursor_line_end(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        app.input_cursor = Some(line_end(&app.input, cursor));
    }

    pub fn move_cursor_buffer_start(&mut self, app: &mut AppState) {
        app.input_cursor = Some(0);
    }

    pub fn move_cursor_buffer_end(&mut self, app: &mut AppState) {
        app.input_cursor = Some(app.input.len());
    }

    pub fn delete_to_line_start(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        let line_start = line_start(&app.input, cursor);
        app.input.replace_range(line_start..cursor, "");
        app.input_cursor = Some(line_start);
        self.last_paste_pending_reveal = false;
    }

    pub fn delete_word_backward(&mut self, app: &mut AppState) {
        let cursor = cursor_index(&app.input, app.input_cursor);
        let end = trim_end_whitespace(&app.input[..cursor]);
        if end == 0 {
            app.input.replace_range(0..cursor, "");
            app.input_cursor = Some(0);
            self.last_paste_pending_reveal = false;
            return;
        }
        let prefix = &app.input[..end];
        let word_start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(idx, ch)| idx + ch.len_utf8());
        app.input.replace_range(word_start..cursor, "");
        app.input_cursor = Some(word_start);
        self.last_paste_pending_reveal = false;
    }

    pub fn expanded_input(&self, input: &str) -> String {
        self.paste_entries
            .iter()
            .fold(input.to_string(), |text, entry| {
                text.replace(&entry.placeholder, &entry.original)
            })
    }

    fn reveal_last_paste(&mut self, input: &mut String) {
        if let Some(entry) = self.paste_entries.last() {
            *input = input.replace(&entry.placeholder, &entry.original);
        }
        self.last_paste_pending_reveal = false;
    }
}

fn image_paste(text: &str) -> Option<(String, &'static str)> {
    let candidates = [
        Some(text.to_string()),
        markdown_image_path(text),
        image_tag_path_attribute(text),
    ];
    for candidate in candidates.into_iter().flatten() {
        let path = normalize_image_path_candidate(&candidate);
        if let Some(mime) = image_mime_for_path(&path) {
            return Some((path, mime));
        }
    }
    None
}

fn trim_end_whitespace(value: &str) -> usize {
    value
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_whitespace())
        .map_or(0, |(idx, ch)| idx + ch.len_utf8())
}

pub(super) fn cursor_index(input: &str, cursor: Option<usize>) -> usize {
    let mut idx = cursor.unwrap_or(input.len()).min(input.len());
    while !input.is_char_boundary(idx) {
        idx = idx.saturating_sub(1);
    }
    idx
}

fn previous_boundary(input: &str, cursor: usize) -> usize {
    input[..cursor]
        .char_indices()
        .next_back()
        .map_or(cursor, |(idx, _)| idx)
}

fn next_boundary(input: &str, cursor: usize) -> usize {
    input[cursor..]
        .chars()
        .next()
        .map_or(cursor, |ch| cursor + ch.len_utf8())
}

fn line_start(input: &str, cursor: usize) -> usize {
    input[..cursor].rfind('\n').map_or(0, |idx| idx + 1)
}

fn line_end(input: &str, cursor: usize) -> usize {
    cursor + input[cursor..].find('\n').unwrap_or(input.len() - cursor)
}

fn markdown_image_path(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("![") {
        return None;
    }
    let start = trimmed.rfind("](")? + 2;
    let path = trimmed.get(start..)?.strip_suffix(')')?;
    Some(path.to_string())
}

fn image_tag_path_attribute(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("<image") {
        return None;
    }
    for quote in ['"', '\''] {
        let needle = format!("path={quote}");
        if let Some(start) = trimmed.find(&needle) {
            let path_start = start + needle.len();
            let path = trimmed.get(path_start..)?.split(quote).next()?;
            return Some(path.to_string());
        }
    }
    None
}

fn normalize_image_path_candidate(candidate: &str) -> String {
    let trimmed = candidate.trim().trim_matches(&['"', '\''][..]);
    let without_scheme = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    percent_decode(without_scheme)
}

fn percent_decode(value: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%'
            && idx + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[idx + 1]), hex_value(bytes[idx + 2]))
        {
            output.push(high * 16 + low);
            idx += 3;
        } else {
            output.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(output).unwrap_or_else(|_| value.to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

pub(super) fn mention_trigger_index(input: &str) -> Option<usize> {
    let idx = input.rfind('@')?;
    let after = &input[idx + 1..];
    if after.contains(char::is_whitespace) {
        return None;
    }
    let before = &input[..idx];
    if before.is_empty() || before.ends_with(char::is_whitespace) {
        Some(idx)
    } else {
        None
    }
}

pub(super) fn mention_trigger_index_at(input: &str, cursor: Option<usize>) -> Option<usize> {
    let cursor = cursor_index(input, cursor);
    mention_trigger_index(&input[..cursor])
}

fn image_mime_for_path(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    match ext {
        "avif" => Some("image/avif"),
        "gif" => Some("image/gif"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "svg" => Some("image/svg+xml"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}
