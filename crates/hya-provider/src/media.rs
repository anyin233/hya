use std::borrow::Cow;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::ProviderError;

const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
const GOOGLE_AUDIO_VIDEO_MIMES: &[&str] = &[
    "video/mp4",
    "video/webm",
    "video/quicktime",
    "audio/wav",
    "audio/mp3",
    "audio/aiff",
    "audio/aac",
    "audio/ogg",
    "audio/flac",
];
const MAX_GOOGLE_MEDIA_ENCODED_BYTES: usize = 28 * 1024 * 1024;
const MAX_GOOGLE_MEDIA_DECODED_BYTES: usize = 20 * 1024 * 1024;
const BASE64_CHUNK_CHARS: usize = 1_024;
const BASE64_CHUNK_BYTES: usize = BASE64_CHUNK_CHARS / 4 * 3;

pub(crate) fn image_url<'a>(
    media_type: &str,
    data: &'a str,
) -> Result<Cow<'a, str>, ProviderError> {
    match image_data(media_type, data)? {
        ImageData::Url(url) => Ok(Cow::Borrowed(url)),
        ImageData::Base64 { .. } if data.starts_with("data:") => Ok(Cow::Borrowed(data)),
        ImageData::Base64 { mime, payload } => {
            Ok(Cow::Owned(format!("data:{mime};base64,{payload}")))
        }
    }
}

enum ImageData<'a> {
    Url(&'a str),
    Base64 {
        mime: &'static str,
        payload: &'a str,
    },
}

fn image_data<'a>(media_type: &str, data: &'a str) -> Result<ImageData<'a>, ProviderError> {
    let mime = IMAGE_MIMES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(media_type))
        .ok_or_else(|| {
            ProviderError::Incompatible(format!(
                "provider does not support media type {media_type}"
            ))
        })?;
    if let Some(rest) = data.strip_prefix("data:") {
        let (header, payload) = rest.split_once(',').ok_or_else(|| {
            ProviderError::Incompatible("image data URL must contain a payload".to_string())
        })?;
        let declared = header.split(';').next().unwrap_or_default();
        if !declared.eq_ignore_ascii_case(mime)
            || !header
                .split(';')
                .skip(1)
                .any(|parameter| parameter.eq_ignore_ascii_case("base64"))
        {
            return Err(ProviderError::Incompatible(format!(
                "image data URL type does not match media type {media_type}"
            )));
        }
        validate_base64(payload)?;
        return Ok(ImageData::Base64 { mime, payload });
    }
    if data.starts_with("https://") || data.starts_with("http://") {
        return Ok(ImageData::Url(data));
    }
    if data.contains("://") {
        return Err(ProviderError::Incompatible(
            "image media must be a data URL, HTTP URL, or base64 payload".to_string(),
        ));
    }
    validate_base64(data)?;
    Ok(ImageData::Base64 {
        mime,
        payload: data,
    })
}

pub(crate) fn google_inline_data(media_type: &str, data: &str) -> Result<Value, ProviderError> {
    let mime = IMAGE_MIMES
        .iter()
        .chain(GOOGLE_AUDIO_VIDEO_MIMES)
        .copied()
        .find(|known| known.eq_ignore_ascii_case(media_type))
        .ok_or_else(|| {
            ProviderError::Incompatible(format!("Google does not support media type {media_type}"))
        })?;
    let payload = if let Some(url) = data.strip_prefix("data:") {
        let (header, payload) = url.split_once(',').ok_or_else(|| {
            ProviderError::Incompatible(
                "Google media data URL must contain valid base64".to_string(),
            )
        })?;
        if !header.split_once(';').is_some_and(|(declared, encoding)| {
            declared.eq_ignore_ascii_case(mime) && encoding.eq_ignore_ascii_case("base64")
        }) {
            return Err(ProviderError::Incompatible(format!(
                "Google media type {media_type} does not match data URL type {header}"
            )));
        }
        payload
    } else {
        data
    };
    if payload.len() > MAX_GOOGLE_MEDIA_ENCODED_BYTES {
        return Err(ProviderError::Incompatible(format!(
            "Google media exceeds the {MAX_GOOGLE_MEDIA_ENCODED_BYTES} byte encoded limit"
        )));
    }
    let decoded = base64_decoded_size(payload).ok_or_else(|| {
        ProviderError::Incompatible("Google media must contain valid base64".to_string())
    })?;
    if decoded > MAX_GOOGLE_MEDIA_DECODED_BYTES {
        return Err(ProviderError::Incompatible(format!(
            "Google media exceeds the {MAX_GOOGLE_MEDIA_DECODED_BYTES} byte decoded limit"
        )));
    }
    Ok(json!({"inlineData":{"mimeType":mime,"data":payload}}))
}

fn base64_decoded_size(data: &str) -> Option<usize> {
    let mut chunks = data.as_bytes().chunks(BASE64_CHUNK_CHARS).peekable();
    let mut scratch = [0_u8; BASE64_CHUNK_BYTES];
    let mut size = 0;
    while let Some(chunk) = chunks.next() {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode_slice(chunk, &mut scratch)
            .ok()?;
        if chunks.peek().is_some() && decoded != BASE64_CHUNK_BYTES {
            return None;
        }
        size += decoded;
    }
    Some(size)
}

fn validate_base64(data: &str) -> Result<(), ProviderError> {
    base64_decoded_size(data).map(|_| ()).ok_or_else(|| {
        ProviderError::Incompatible("image media must contain valid base64".to_string())
    })
}

pub(crate) fn anthropic_image_source(media_type: &str, data: &str) -> Result<Value, ProviderError> {
    match image_data(media_type, data)? {
        ImageData::Base64 { mime, payload } => Ok(json!({
            "type": "base64", "media_type": mime, "data": payload,
        })),
        ImageData::Url(url) => Ok(json!({"type": "url", "url": url})),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn google_media_preserves_canonical_payload_and_enforces_limits() {
        let payload = format!("{}Zg==", "A".repeat(BASE64_CHUNK_CHARS));
        let data_url = format!("data:Audio/WAV;BASE64,{payload}");
        assert_eq!(
            google_inline_data("audio/wav", &data_url).unwrap(),
            json!({"inlineData":{"mimeType":"audio/wav","data":payload}})
        );
        assert!(google_inline_data("audio/wav", "data:audio/wav;foo;base64,Zg==").is_err());
        assert!(google_inline_data("audio/wav", "Zg").is_err());
        assert!(google_inline_data("audio/wav", "Zh==").is_err());
        assert!(google_inline_data("audio/wav", "Zg==AAAA").is_err());

        let size = MAX_GOOGLE_MEDIA_DECODED_BYTES.div_ceil(3) * 4;
        let mut boundary = "A".repeat(size);
        assert!(
            google_inline_data("image/png", &boundary)
                .unwrap_err()
                .to_string()
                .contains("decoded limit")
        );
        boundary.pop();
        boundary.push('=');
        assert!(google_inline_data("image/png", &boundary).is_ok());
        let oversized = "A".repeat(MAX_GOOGLE_MEDIA_ENCODED_BYTES + 1);
        assert!(
            google_inline_data("image/png", &oversized)
                .unwrap_err()
                .to_string()
                .contains("encoded limit")
        );
    }

    #[test]
    fn image_data_url_preserves_payload_and_rejects_mismatches() {
        let data = "data:image/png;base64,aGVsbG8=";
        assert_eq!(image_url("image/png", data).unwrap().as_ref(), data);
        assert!(image_url("image/jpeg", data).is_err());
        assert!(image_url("audio/wav", "ZGF0YQ==").is_err());
    }

    #[test]
    fn base64_padding_is_only_valid_in_the_last_chunk() {
        let invalid = format!("{}AA==AAAA", "A".repeat(1_020));
        assert!(image_url("image/png", &invalid).is_err());
        let valid = "A".repeat(1_028);
        assert_eq!(
            image_url("image/png", &valid).unwrap(),
            format!("data:image/png;base64,{valid}")
        );
    }

    #[test]
    fn raw_base64_becomes_a_data_url_without_fetching() {
        assert_eq!(
            image_url("image/png", "aGVsbG8=").unwrap().as_ref(),
            "data:image/png;base64,aGVsbG8="
        );
        assert!(image_url("image/png", "/tmp/image.png").is_err());
    }
}
