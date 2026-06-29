use std::path::Path;

pub(super) fn for_path(path: &Path, bytes: Option<&[u8]>) -> &'static str {
    if let Some(mime) = explicit_extension(path) {
        return mime;
    }
    if let Some(mime) = mime_guess::from_path(path).first_raw() {
        return mime;
    }
    bytes.map_or("application/octet-stream", sniff)
}

pub(super) fn sniff(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
        return "image/gif";
    }
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    "application/octet-stream"
}

fn explicit_extension(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt") => Some("text/plain"),
        Some("rs") => Some("application/rls-services+xml"),
        Some("toml") => Some("application/toml"),
        Some("yaml" | "yml") => Some("text/yaml"),
        Some("ts" | "mts") => Some("video/mp2t"),
        Some("tsx" | "cts" | "py" | "go" | "hpp") => Some("application/octet-stream"),
        Some("js" | "mjs") => Some("text/javascript"),
        Some("jsx") => Some("text/jsx"),
        Some("cjs") => Some("application/node"),
        Some("css") => Some("text/css"),
        Some("html") => Some("text/html"),
        Some("java") => Some("text/x-java-source"),
        Some("c" | "h" | "cpp") => Some("text/x-c"),
        Some("sh") => Some("application/x-sh"),
        Some("csv") => Some("text/csv"),
        Some("md") => Some("text/markdown"),
        Some("mdx") => Some("text/mdx"),
        Some("json") => Some("application/json"),
        Some("xml") => Some("application/xml"),
        Some("svg") => Some("image/svg+xml"),
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("ico") => Some("image/vnd.microsoft.icon"),
        Some("pdf") => Some("application/pdf"),
        Some("wasm") => Some("application/wasm"),
        Some("mp3") => Some("audio/mpeg"),
        Some("m4a") => Some("audio/mp4"),
        Some("wav") => Some("audio/wav"),
        Some("ogg") => Some("audio/ogg"),
        Some("mp4") => Some("video/mp4"),
        Some("webm") => Some("video/webm"),
        Some("ipynb") => Some("application/x-ipynb+json"),
        _ => None,
    }
}
