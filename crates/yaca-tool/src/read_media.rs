use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::lsp_path::display_path;

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
    let bytes = tokio::fs::read(path).await?;
    let message = if is_pdf_attachment(mime) {
        "PDF read successfully"
    } else {
        "Image read successfully"
    };
    Ok(json!({
        "title": relative_title(path, workdir),
        "output": message,
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
