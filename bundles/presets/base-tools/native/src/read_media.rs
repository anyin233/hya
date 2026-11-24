use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hya_tool::MAX_CODING_ATTACHMENT_BYTES;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use hya_tool::path::display_path;

const SAMPLE_BYTES: usize = 4096;

pub(crate) enum ReadFileKind {
    Text,
    Binary,
    Attachment(String),
}

pub(crate) async fn classify_file(path: &Path) -> Result<ReadFileKind, std::io::Error> {
    let sample = read_sample(path).await?;
    let mime = sniff_attachment_mime(&sample, fallback_mime(path));
    if is_supported_image(mime) || is_pdf_attachment(mime) {
        return Ok(ReadFileKind::Attachment(mime.to_string()));
    }
    if is_binary_file(path, &sample) {
        return Ok(ReadFileKind::Binary);
    }
    Ok(ReadFileKind::Text)
}

pub(crate) async fn attachment_value(
    path: &Path,
    workdir: &Path,
    mime: &str,
) -> Result<Value, std::io::Error> {
    if !is_pdf_attachment(mime) {
        let size = tokio::fs::metadata(path).await?.len();
        let limit = max_image_bytes(mime);
        if size > limit {
            return Ok(json!({
                "title": relative_title(path, workdir),
                "output": format!(
                    "Image file ({mime}, {size} bytes). It is larger than the limit of {limit} \
                     bytes for an image in a tool result, so it is not attached. Downscale or \
                     crop it with a program and read the smaller file, or inspect it with a \
                     program."
                ),
                "metadata": {
                    "preview": "Image too large to attach",
                    "truncated": false,
                    "loaded": [],
                },
            }));
        }
    }
    let bytes = tokio::fs::read(path).await?;
    let bytes = if let Some(format) = image::ImageFormat::from_mime_type(mime) {
        let (bytes, decoded) = tokio::task::spawn_blocking(move || {
            let decoded = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
                .decode()
                .map(drop);
            (bytes, decoded)
        })
        .await
        .map_err(std::io::Error::other)?;
        if let Err(error) = decoded {
            return Ok(json!({
                "title": relative_title(path, workdir),
                "output": format!("Malformed or incomplete image ({mime}, {} bytes). Decoding failed: {error}. Check the program that wrote the file.", bytes.len()),
                "metadata": {"preview": "Image could not be decoded", "truncated": false, "loaded": []},
            }));
        }
        bytes
    } else {
        bytes
    };
    let message = if is_pdf_attachment(mime) {
        "PDF read successfully"
    } else {
        "Image read successfully"
    };
    // A PDF never reaches the model, so the model is told so here. An image
    // goes to a model that takes images; the provider encoder either sends it
    // or says that it is not shown, per request.
    let output = if is_pdf_attachment(mime) {
        format!(
            "This file is a PDF ({} bytes). Its content is not shown to you. Extract the text \
             with a program instead.",
            bytes.len()
        )
    } else {
        format!("Image file ({mime}, {} bytes).", bytes.len())
    };
    Ok(json!({
        "title": relative_title(path, workdir),
        "output": output,
        "metadata": {
            "preview": message,
            "truncated": false,
            "loaded": [],
        },
        "attachments": [{
            "type": "file",
            "mime": mime,
            "url": format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
        }],
    }))
}

/// Largest image file whose one-entry attachment array (base64 data URL)
/// fits the tool-result attachment budget.
fn max_image_bytes(mime: &str) -> u64 {
    let empty = json!([{"type": "file", "mime": mime, "url": format!("data:{mime};base64,")}]);
    let base64 = MAX_CODING_ATTACHMENT_BYTES.saturating_sub(empty.to_string().len());
    (base64 / 4 * 3) as u64
}

async fn read_sample(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut sample = vec![0; SAMPLE_BYTES];
    let size = file.read(&mut sample).await?;
    sample.truncate(size);
    Ok(sample)
}

fn sniff_attachment_mime<'a>(bytes: &[u8], fallback: &'a str) -> &'a str {
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
        return "image/gif";
    }
    if bytes.starts_with(&[0x25, 0x50, 0x44, 0x46, 0x2d]) {
        return "application/pdf";
    }
    if bytes.starts_with(&[0x52, 0x49, 0x46, 0x46])
        && bytes
            .get(8..)
            .is_some_and(|rest| rest.starts_with(&[0x57, 0x45, 0x42, 0x50]))
    {
        return "image/webp";
    }
    fallback
}

fn fallback_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn is_pdf_attachment(mime: &str) -> bool {
    mime == "application/pdf"
}

fn is_supported_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn is_binary_file(path: &Path, bytes: &[u8]) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "zip"
            | "tar"
            | "gz"
            | "exe"
            | "dll"
            | "so"
            | "class"
            | "jar"
            | "war"
            | "7z"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "bin"
            | "dat"
            | "obj"
            | "o"
            | "a"
            | "lib"
            | "wasm"
            | "pyc"
            | "pyo"
    ) {
        return true;
    }
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let non_printable = bytes
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    non_printable * 10 > bytes.len() * 3
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn corrupt_png_is_explained_without_an_attachment() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("hya-read-{}", hya_proto::SessionId::new()));
        tokio::fs::create_dir(&dir).await?;
        let path = dir.join("corrupt.png");
        tokio::fs::write(&path, b"\x89PNG\r\n\x1a\n").await?;
        let result = attachment_value(&path, &dir, "image/png").await?;
        tokio::fs::remove_dir_all(dir).await?;
        assert_eq!(result["title"], "corrupt.png");
        assert!(
            result["output"]
                .as_str()
                .is_some_and(|text| text.contains("Malformed"))
        );
        assert!(result.get("attachments").is_none());
        Ok(())
    }
}
