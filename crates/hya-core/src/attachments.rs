//! Prompt image attachments.
//!
//! A prompt turn may carry images. Admission validates them (image types
//! only, [`MAX_ATTACHMENT_BYTES`] each, [`MAX_TURN_ATTACHMENT_BYTES`] per
//! turn), stores each image once in the session's content-addressed blob
//! table, and records one `UserPromptContextRecorded` file entry per image
//! that names the blob by its sha256 hash — the event log never holds the
//! bytes. The entries are appended in the same transaction as the user
//! message, so a prompt is never visible without its images.
//!
//! Replay stays deterministic: the projection folds only the entries (hash,
//! name, type, size); a model request hydrates each entry from the blob into
//! a `data:` URL image part.

use base64::Engine as _;
use hya_proto::PartId;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// Largest single attachment, in bytes (10 MiB).
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Largest total of one turn's attachments, in bytes (20 MiB).
pub const MAX_TURN_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// Image types a prompt may attach (what every image-capable provider route
/// encodes).
pub const IMAGE_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// `type` of a prompt image entry in `UserPromptContextRecorded.files`.
const ENTRY_TYPE: &str = "image_attachment";

/// One image attached to a prompt turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptAttachment {
    /// File name shown in the transcript and sent to the model.
    pub name: String,
    /// MIME type; empty to detect it from the bytes.
    pub mime: String,
    /// The image bytes.
    pub data: Vec<u8>,
    /// Client-side path the image was read from (display only).
    pub path: Option<String>,
}

/// Why prompt attachments were refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttachmentError {
    /// The attachment has no name.
    #[error("attachment {index} has no name")]
    MissingName {
        /// Position in the request.
        index: usize,
    },
    /// The attachment has no bytes.
    #[error("attachment `{name}` is empty")]
    Empty {
        /// Attachment name.
        name: String,
    },
    /// The declared type is not a supported image type.
    #[error(
        "attachment `{name}` has unsupported type `{mime}`; supported: image/png, image/jpeg, image/gif, image/webp"
    )]
    UnsupportedType {
        /// Attachment name.
        name: String,
        /// Declared type.
        mime: String,
    },
    /// The bytes are not a supported image.
    #[error("attachment `{name}` is not a PNG, JPEG, GIF, or WebP image")]
    NotAnImage {
        /// Attachment name.
        name: String,
    },
    /// The declared type does not match the bytes.
    #[error("attachment `{name}` is declared `{declared}` but its bytes are `{detected}`")]
    TypeMismatch {
        /// Attachment name.
        name: String,
        /// Declared type.
        declared: String,
        /// Type detected from the bytes.
        detected: &'static str,
    },
    /// One attachment is over [`MAX_ATTACHMENT_BYTES`].
    #[error("attachment `{name}` is {size} bytes; the limit is {limit} bytes (10 MiB)")]
    TooLarge {
        /// Attachment name.
        name: String,
        /// Its size.
        size: usize,
        /// The per-attachment limit.
        limit: usize,
    },
    /// The turn's attachments total over [`MAX_TURN_ATTACHMENT_BYTES`].
    #[error("attachments total {total} bytes; the per-turn limit is {limit} bytes (20 MiB)")]
    TurnTooLarge {
        /// Their total size.
        total: usize,
        /// The per-turn limit.
        limit: usize,
    },
}

/// The image type of `bytes` from its signature, if it is a supported image.
#[must_use]
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Validate a turn's attachments and normalize each `mime` to the detected
/// image type.
///
/// # Errors
/// The first [`AttachmentError`] found; nothing should be admitted then.
pub fn validate_prompt_attachments(
    attachments: &mut [PromptAttachment],
) -> Result<(), AttachmentError> {
    let mut total = 0usize;
    for (index, attachment) in attachments.iter_mut().enumerate() {
        let name = attachment.name.trim();
        if name.is_empty() {
            return Err(AttachmentError::MissingName { index });
        }
        attachment.name = name.to_string();
        let name = attachment.name.clone();
        if attachment.data.is_empty() {
            return Err(AttachmentError::Empty { name });
        }
        let declared = attachment.mime.trim().to_ascii_lowercase();
        if !declared.is_empty() && !IMAGE_MIME_TYPES.contains(&declared.as_str()) {
            return Err(AttachmentError::UnsupportedType {
                name,
                mime: attachment.mime.clone(),
            });
        }
        let size = attachment.data.len();
        if size > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::TooLarge {
                name,
                size,
                limit: MAX_ATTACHMENT_BYTES,
            });
        }
        let detected = sniff_image_mime(&attachment.data)
            .ok_or(AttachmentError::NotAnImage { name: name.clone() })?;
        if !declared.is_empty() && declared != detected {
            return Err(AttachmentError::TypeMismatch {
                name,
                declared,
                detected,
            });
        }
        attachment.mime = detected.to_string();
        attachment.path = attachment
            .path
            .take()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty());
        total = total.saturating_add(size);
    }
    if total > MAX_TURN_ATTACHMENT_BYTES {
        return Err(AttachmentError::TurnTooLarge {
            total,
            limit: MAX_TURN_ATTACHMENT_BYTES,
        });
    }
    Ok(())
}

/// Lowercase hex sha256 of `bytes` (the blob key).
#[must_use]
pub fn blob_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `data:` URL of an image.
#[must_use]
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// The `UserPromptContextRecorded.files` entry for one validated attachment
/// stored under `hash`; `part` is the id its transcript part carries.
#[must_use]
pub(crate) fn file_entry(part: PartId, attachment: &PromptAttachment, hash: &str) -> Value {
    let mut entry = json!({
        "type": ENTRY_TYPE,
        "part": part.to_string(),
        "name": attachment.name,
        "mime": attachment.mime,
        "size": attachment.data.len(),
        "blob": hash,
    });
    if let (Some(path), Some(map)) = (&attachment.path, entry.as_object_mut()) {
        map.insert("path".to_string(), Value::String(path.clone()));
    }
    entry
}

/// A prompt image as recorded on a user message (no bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedAttachment {
    /// Transcript part id.
    pub part: String,
    /// File name.
    pub name: String,
    /// Image type.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
    /// sha256 of the bytes (the session blob key).
    pub blob: String,
    /// Client-side path, when sent.
    pub path: Option<String>,
}

impl RecordedAttachment {
    /// Parse one `UserPromptContextRecorded.files` entry; `None` for entries
    /// that are not prompt images (for example `@file` context).
    #[must_use]
    pub fn from_entry(entry: &Value) -> Option<Self> {
        if entry.get("type").and_then(Value::as_str) != Some(ENTRY_TYPE) {
            return None;
        }
        let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            part: text("part")?,
            name: text("name")?,
            mime: text("mime")?,
            size: entry.get("size").and_then(Value::as_u64).unwrap_or(0),
            blob: text("blob")?,
            path: text("path"),
        })
    }
}

/// The prompt images recorded in a message's `files`, in order.
#[must_use]
pub fn recorded_attachments(files: &[Value]) -> Vec<RecordedAttachment> {
    files
        .iter()
        .filter_map(RecordedAttachment::from_entry)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00";

    fn image(name: &str, mime: &str, data: &[u8]) -> PromptAttachment {
        PromptAttachment {
            name: name.to_string(),
            mime: mime.to_string(),
            data: data.to_vec(),
            path: None,
        }
    }

    #[test]
    fn sniffs_every_supported_type() {
        assert_eq!(sniff_image_mime(PNG), Some("image/png"));
        assert_eq!(sniff_image_mime(b"\xff\xd8\xff\xdb"), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"GIF89a.."), Some("image/gif"));
        assert_eq!(
            sniff_image_mime(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_image_mime(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(sniff_image_mime(b"<svg/>"), None);
    }

    #[test]
    fn validation_normalizes_the_type_and_enforces_limits() {
        let mut ok = [image(" a.png ", "IMAGE/PNG", PNG), image("b", "", PNG)];
        validate_prompt_attachments(&mut ok).unwrap();
        assert_eq!(ok[0].name, "a.png");
        assert_eq!(ok[0].mime, "image/png");
        assert_eq!(ok[1].mime, "image/png");

        let mut big = PNG.to_vec();
        big.resize(MAX_ATTACHMENT_BYTES + 1, 0);
        assert!(matches!(
            validate_prompt_attachments(&mut [image("big", "image/png", &big)]),
            Err(AttachmentError::TooLarge { .. })
        ));
        let mut half = PNG.to_vec();
        half.resize(MAX_ATTACHMENT_BYTES, 0);
        let mut three = [
            image("1", "", &half),
            image("2", "", &half),
            image("3", "", PNG),
        ];
        assert!(matches!(
            validate_prompt_attachments(&mut three),
            Err(AttachmentError::TurnTooLarge { .. })
        ));
        assert!(matches!(
            validate_prompt_attachments(&mut [image("x", "image/jpeg", PNG)]),
            Err(AttachmentError::TypeMismatch { .. })
        ));
        assert!(matches!(
            validate_prompt_attachments(&mut [image("x", "application/pdf", PNG)]),
            Err(AttachmentError::UnsupportedType { .. })
        ));
    }

    #[test]
    fn entries_round_trip_without_bytes() {
        let part = PartId::new();
        let mut attachment = image("a.png", "image/png", PNG);
        attachment.path = Some("/tmp/a.png".to_string());
        let hash = blob_hash(PNG);
        let entry = file_entry(part, &attachment, &hash);
        assert!(!entry.to_string().contains("base64"));
        let recorded = recorded_attachments(&[entry, json!({"mime": "text/plain"})]);
        assert_eq!(
            recorded,
            vec![RecordedAttachment {
                part: part.to_string(),
                name: "a.png".to_string(),
                mime: "image/png".to_string(),
                size: PNG.len() as u64,
                blob: hash,
                path: Some("/tmp/a.png".to_string()),
            }]
        );
    }
}
