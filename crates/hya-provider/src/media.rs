use std::borrow::Cow;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::ProviderError;

const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
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

fn validate_base64(data: &str) -> Result<(), ProviderError> {
    let bytes = data.as_bytes();
    let mut scratch = [0_u8; BASE64_CHUNK_BYTES];
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk_len = (bytes.len() - offset).min(BASE64_CHUNK_CHARS);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode_slice(&bytes[offset..offset + chunk_len], &mut scratch)
            .map_err(|_| {
                ProviderError::Incompatible("image media must contain valid base64".to_string())
            })?;
        if offset + chunk_len < bytes.len() && decoded != BASE64_CHUNK_BYTES {
            return Err(ProviderError::Incompatible(
                "image media must contain valid base64".to_string(),
            ));
        }
        offset += chunk_len;
    }
    Ok(())
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
