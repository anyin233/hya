//! Image attachment checks shared by the bare and full read tools.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, ImageResult};
use serde_json::json;
use tokio::task::JoinError;

use crate::MAX_CODING_ATTACHMENT_BYTES;

/// Identify a supported image by its file signature and return its MIME type and decoder format.
#[must_use]
pub fn sniff_image_mime(bytes: &[u8]) -> Option<(&'static str, ImageFormat)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", ImageFormat::Png))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", ImageFormat::Jpeg))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", ImageFormat::Gif))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", ImageFormat::WebP))
    } else {
        None
    }
}

/// Largest image file whose one-entry attachment array fits the tool-result budget.
#[must_use]
pub fn max_image_attachment_bytes(mime: &str) -> usize {
    let empty = json!([{"type": "file", "mime": mime, "url": format!("data:{mime};base64,")}]);
    let base64 = MAX_CODING_ATTACHMENT_BYTES.saturating_sub(empty.to_string().len());
    // Padded base64 uses four bytes for every three bytes of file data.
    base64 / 4 * 3
}

/// Decode on a blocking thread and return the original bytes with the decoder result.
///
/// The outer error indicates that the decode task failed to run to completion.
pub async fn validate_image_decode(
    bytes: Vec<u8>,
    format: ImageFormat,
) -> Result<(Vec<u8>, ImageResult<()>), JoinError> {
    tokio::task::spawn_blocking(move || {
        let decoded = ImageReader::with_format(Cursor::new(&bytes), format)
            .decode()
            .map(drop);
        (bytes, decoded)
    })
    .await
}
