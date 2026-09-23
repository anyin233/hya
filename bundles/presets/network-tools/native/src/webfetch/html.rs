pub(crate) fn to_markdown(html: &str) -> String {
    let text = strip_html(html, true);
    collapse_blank_lines(&text)
}

pub(crate) fn to_text(html: &str) -> String {
    let text = strip_html(html, false);
    collapse_blank_lines(&text)
}

fn strip_html(html: &str, headings: bool) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    let mut skip: Option<&'static str> = None;
    let mut text = String::new();

    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                handle_tag(&mut out, &tag, &mut skip, headings);
                tag.clear();
                in_tag = false;
            } else {
                tag.push(ch);
            }
            continue;
        }

        if ch == '<' {
            if skip.is_none() && !text.is_empty() {
                out.push_str(&decode_entities(&text));
                text.clear();
            } else {
                text.clear();
            }
            in_tag = true;
        } else {
            text.push(ch);
        }
    }
    if skip.is_none() && !text.is_empty() {
        out.push_str(&decode_entities(&text));
    }
    out
}

fn handle_tag(out: &mut String, raw: &str, skip: &mut Option<&'static str>, headings: bool) {
    let normalized = raw.trim().trim_start_matches('/').trim();
    let name = normalized
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let closing = raw.trim_start().starts_with('/');

    if matches!(name.as_str(), "script" | "style" | "noscript") {
        if closing && skip.is_some_and(|current| current == name) {
            *skip = None;
        } else if !closing {
            *skip = match name.as_str() {
                "script" => Some("script"),
                "style" => Some("style"),
                "noscript" => Some("noscript"),
                _ => None,
            };
        }
        return;
    }
    if skip.is_some() {
        return;
    }

    match name.as_str() {
        "h1" if !closing && headings => out.push_str("\n# "),
        "h2" if !closing && headings => out.push_str("\n## "),
        "h3" if !closing && headings => out.push_str("\n### "),
        "p" | "div" | "section" | "article" | "br" | "li" | "tr" | "h1" | "h2" | "h3"
            if closing || name == "br" =>
        {
            out.push('\n');
        }
        _ => {}
    }
}

fn collapse_blank_lines(input: &str) -> String {
    let mut out = String::new();
    let mut previous_blank = true;
    for line in input.lines().map(str::trim) {
        if line.is_empty() {
            if !previous_blank {
                out.push('\n');
                previous_blank = true;
            }
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        previous_blank = false;
    }
    out.trim().to_string()
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}
