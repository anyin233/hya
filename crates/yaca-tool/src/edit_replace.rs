use crate::tool::ToolError;

mod replacers;

pub(crate) struct Replacement {
    pub(crate) content: String,
    pub(crate) replaced: usize,
}

pub(crate) fn replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<Replacement, ToolError> {
    if old == new {
        return Err(ToolError::Other(
            "No changes to apply: oldString and newString are identical.".to_string(),
        ));
    }
    if old.is_empty() {
        return Err(ToolError::Other(
            "oldString cannot be empty when editing an existing file. Provide the exact text to replace, or use write for an intentional full-file replacement."
                .to_string(),
        ));
    }

    let ending = detect_line_ending(content);
    let old = convert_to_line_ending(&normalize_line_endings(old), ending);
    let new = convert_to_line_ending(&normalize_line_endings(new), ending);
    let mut matched_any_candidate = false;

    for search in replacers::candidates(content, &old) {
        let Some(index) = content.find(&search) else {
            continue;
        };
        matched_any_candidate = true;
        if is_disproportionate_match(&search, &old) {
            return Err(ToolError::Other(
                "Refusing replacement because the matched span is much larger than oldString. Re-read the file and provide the full exact oldString for the intended replacement."
                    .to_string(),
            ));
        }
        if replace_all {
            let replaced = content.matches(&search).count();
            return Ok(Replacement {
                content: content.replace(&search, &new),
                replaced,
            });
        }
        if content
            .rfind(&search)
            .is_some_and(|last_index| index == last_index)
        {
            return Ok(Replacement {
                content: replace_once(content, index, &search, &new),
                replaced: 1,
            });
        }
    }

    if matched_any_candidate {
        return Err(ToolError::Other(
            "Found multiple matches for oldString. Provide more surrounding context to make the match unique."
                .to_string(),
        ));
    }
    Err(ToolError::Other(
        "Could not find oldString in the file. It must match exactly, including whitespace, indentation, and line endings."
            .to_string(),
    ))
}

fn replace_once(content: &str, index: usize, search: &str, new: &str) -> String {
    let mut out = String::with_capacity(content.len() - search.len() + new.len());
    out.push_str(&content[..index]);
    out.push_str(new);
    out.push_str(&content[index + search.len()..]);
    out
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn detect_line_ending(text: &str) -> &str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

fn convert_to_line_ending(text: &str, ending: &str) -> String {
    if ending == "\n" {
        return text.to_string();
    }
    text.replace('\n', ending)
}

fn is_disproportionate_match(search: &str, old: &str) -> bool {
    let old_lines = old.split('\n').count();
    let search_lines = search.split('\n').count();
    if search_lines >= (old_lines + 3).max(old_lines * 2) {
        return true;
    }
    if old_lines == 1 {
        return false;
    }
    search.trim().len() > (old.trim().len() + 500).max(old.trim().len() * 4)
}
