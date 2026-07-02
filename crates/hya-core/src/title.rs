use hya_proto::Projection;
use time::OffsetDateTime;
use time::macros::format_description;

const FALLBACK_PREFIX: &str = "Untitled Session_";
const COMPAT_ROOT_PREFIX: &str = "New session - ";
const COMPAT_CHILD_PREFIX: &str = "Child session - ";
const TITLE_LIMIT: usize = 100;
const TITLE_FORMAT: &[time::format_description::FormatItem<'_>] =
    format_description!("[year]-[month]-[day]-[hour]-[minute]");

#[must_use]
pub fn fallback_title(activity_millis: i64) -> String {
    let seconds = activity_millis.div_euclid(1000);
    let Ok(time) = OffsetDateTime::from_unix_timestamp(seconds) else {
        return format!("{FALLBACK_PREFIX}1970-01-01-00-00");
    };
    let timestamp = time
        .format(TITLE_FORMAT)
        .unwrap_or_else(|_| "1970-01-01-00-00".to_string());
    format!("{FALLBACK_PREFIX}{timestamp}")
}

#[must_use]
pub fn is_default_or_fallback_title(title: &str) -> bool {
    is_hya_fallback_title(title)
        || has_compat_timestamp_prefix(title, COMPAT_ROOT_PREFIX)
        || has_compat_timestamp_prefix(title, COMPAT_CHILD_PREFIX)
}

#[must_use]
pub fn clean_title_output(output: &str) -> Option<String> {
    strip_think_blocks(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(truncate_title)
}

#[must_use]
pub fn is_empty_unnamed_session(projection: &Projection) -> bool {
    projection.session.id.is_some()
        && projection.session.title.is_none()
        && projection.session.messages.is_empty()
}

fn is_hya_fallback_title(title: &str) -> bool {
    let Some(timestamp) = title.strip_prefix(FALLBACK_PREFIX) else {
        return false;
    };
    timestamp.len() == "YYYY-MM-DD-HH-MM".len()
        && timestamp.bytes().enumerate().all(|(idx, byte)| match idx {
            4 | 7 | 10 | 13 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn has_compat_timestamp_prefix(title: &str, prefix: &str) -> bool {
    let Some(timestamp) = title.strip_prefix(prefix) else {
        return false;
    };
    OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339).is_ok()
}

fn strip_think_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + "<think>".len()..];
        let Some(end) = after_start.find("</think>") else {
            rest = "";
            break;
        };
        rest = &after_start[end + "</think>".len()..];
    }
    out.push_str(rest);
    out
}

fn truncate_title(title: &str) -> String {
    let mut chars = title.chars();
    let truncated: String = chars.by_ref().take(TITLE_LIMIT).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use hya_proto::{MessageId, MessageProjection, Projection, Role, SessionId};

    use super::*;

    #[test]
    fn fallback_title_uses_utc_activity_minute() {
        assert_eq!(
            fallback_title(1_700_000_000_000),
            "Untitled Session_2023-11-14-22-13"
        );
    }

    #[test]
    fn default_title_detector_accepts_hya_and_compat_defaults() {
        assert!(is_default_or_fallback_title(
            "Untitled Session_2023-11-14-22-13"
        ));
        assert!(is_default_or_fallback_title(
            "New session - 2026-06-28T12:34:56.789Z"
        ));
        assert!(is_default_or_fallback_title(
            "Child session - 2026-06-28T12:34:56Z"
        ));
        assert!(!is_default_or_fallback_title("New session - roadmap"));
    }

    #[test]
    fn clean_title_output_strips_think_blocks_and_uses_first_non_empty_line() {
        assert_eq!(
            clean_title_output("<think>hidden\nreasoning</think>\n\nUseful title\nSecond line")
                .as_deref(),
            Some("Useful title")
        );
    }

    #[test]
    fn clean_title_output_truncates_to_100_chars_with_ellipsis() {
        let long = "a".repeat(101);
        let expected = format!("{}...", "a".repeat(100));

        assert_eq!(clean_title_output(&long), Some(expected));
    }

    #[test]
    fn empty_unnamed_session_predicate_tracks_title_and_messages() {
        let mut projection = Projection::default();
        projection.session.id = Some(SessionId::new());

        assert!(is_empty_unnamed_session(&projection));

        projection.session.title = Some("Manual title".to_string());
        assert!(!is_empty_unnamed_session(&projection));

        projection.session.title = None;
        projection.session.messages.push(MessageProjection {
            id: MessageId::new(),
            role: Role::User,
            finish: None,
            tokens: None,
            files: Vec::new(),
            agents: Vec::new(),
            parts: Vec::new(),
        });
        assert!(!is_empty_unnamed_session(&projection));
    }
}
