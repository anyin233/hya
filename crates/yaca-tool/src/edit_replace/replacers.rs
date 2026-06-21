pub(crate) fn candidates(content: &str, find: &str) -> Vec<String> {
    let mut candidates = vec![find.to_string()];
    candidates.extend(line_trimmed(content, find));
    candidates.extend(whitespace_normalized(content, find));
    candidates
}

fn line_trimmed(content: &str, find: &str) -> Vec<String> {
    let original_lines: Vec<_> = content.split('\n').collect();
    let mut search_lines: Vec<_> = find.split('\n').collect();
    if search_lines.last().is_some_and(|line| line.is_empty()) {
        search_lines.pop();
    }
    if search_lines.is_empty() || original_lines.len() < search_lines.len() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for start in 0..=original_lines.len() - search_lines.len() {
        if !search_lines
            .iter()
            .enumerate()
            .all(|(offset, search)| original_lines[start + offset].trim() == search.trim())
        {
            continue;
        }
        let match_start = byte_index_for_line(&original_lines, start);
        let match_end = byte_index_for_line(&original_lines, start + search_lines.len()) - 1;
        matches.push(content[match_start..match_end].to_string());
    }
    matches
}

fn whitespace_normalized(content: &str, find: &str) -> Vec<String> {
    let normalized_find = normalize_whitespace(find);
    let mut matches = Vec::new();
    let lines: Vec<_> = content.split('\n').collect();

    for line in &lines {
        let normalized_line = normalize_whitespace(line);
        if normalized_line == normalized_find {
            matches.push((*line).to_string());
        } else if normalized_line.contains(&normalized_find) {
            let words: Vec<_> = find.split_whitespace().collect();
            if let Some(hit) = find_words_with_whitespace(line, &words) {
                matches.push(hit);
            }
        }
    }

    let find_lines: Vec<_> = find.split('\n').collect();
    if find_lines.len() > 1 && lines.len() >= find_lines.len() {
        for start in 0..=lines.len() - find_lines.len() {
            let block = lines[start..start + find_lines.len()].join("\n");
            if normalize_whitespace(&block) == normalized_find {
                matches.push(block);
            }
        }
    }

    matches
}

fn find_words_with_whitespace(line: &str, words: &[&str]) -> Option<String> {
    let first = words.first()?;
    for (start, _) in line.char_indices() {
        let mut pos = start;
        if !line[pos..].starts_with(first) {
            continue;
        }
        let mut matched = true;
        for (index, word) in words.iter().enumerate() {
            if !line[pos..].starts_with(word) {
                matched = false;
                break;
            }
            pos += word.len();
            if index == words.len() - 1 {
                break;
            }
            let Some(next) = consume_required_whitespace(line, pos) else {
                matched = false;
                break;
            };
            pos = next;
        }
        if matched {
            return Some(line[start..pos].to_string());
        }
    }
    None
}

fn consume_required_whitespace(line: &str, pos: usize) -> Option<usize> {
    let mut saw_whitespace = false;
    let mut end = pos;
    for (offset, ch) in line[pos..].char_indices() {
        if !ch.is_whitespace() {
            break;
        }
        saw_whitespace = true;
        end = pos + offset + ch.len_utf8();
    }
    saw_whitespace.then_some(end)
}

fn byte_index_for_line(lines: &[&str], line: usize) -> usize {
    lines.iter().take(line).map(|line| line.len() + 1).sum()
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
