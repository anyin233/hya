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
        self.last_paste_pending_reveal = false;
        PasteOutcome {
            refresh_popup: true,
        }
    }

    pub fn clear(&mut self, app: &mut AppState) {
        app.input.clear();
        app.attachments.clear();
        self.paste_entries.clear();
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
