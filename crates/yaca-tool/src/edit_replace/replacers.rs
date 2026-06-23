pub(crate) fn candidates(content: &str, find: &str) -> Vec<String> {
    let mut candidates = vec![find.to_string()];
    candidates.extend(line_trimmed(content, find));
    candidates.extend(block_anchor(content, find));
    candidates.extend(whitespace_normalized(content, find));
    candidates.extend(indentation_flexible(content, find));
    candidates.extend(escape_normalized(content, find));
    candidates.extend(trimmed_boundary(content, find));
    candidates.extend(context_aware(content, find));
    candidates.extend(multi_occurrence(content, find));
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

fn block_anchor(content: &str, find: &str) -> Vec<String> {
    const SINGLE_CANDIDATE_SIMILARITY_THRESHOLD: f64 = 0.65;
    const MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD: f64 = 0.65;

    let original_lines: Vec<_> = content.split('\n').collect();
    let mut search_lines: Vec<_> = find.split('\n').collect();
    if search_lines.len() < 3 {
        return Vec::new();
    }
    if search_lines.last().is_some_and(|line| line.is_empty()) {
        search_lines.pop();
    }

    let Some(first_line_search) = search_lines.first().map(|line| line.trim()) else {
        return Vec::new();
    };
    let Some(last_line_search) = search_lines.last().map(|line| line.trim()) else {
        return Vec::new();
    };
    let search_block_size = search_lines.len();
    let max_line_delta = (search_block_size / 4).max(1);
    let mut candidates = Vec::new();

    for start_line in 0..original_lines.len() {
        if original_lines[start_line].trim() != first_line_search {
            continue;
        }
        for (end_line, line) in original_lines
            .iter()
            .enumerate()
            .skip(start_line.saturating_add(2))
        {
            if line.trim() == last_line_search {
                let actual_block_size = end_line - start_line + 1;
                if actual_block_size.abs_diff(search_block_size) <= max_line_delta {
                    candidates.push((start_line, end_line));
                }
                break;
            }
        }
    }

    match candidates.as_slice() {
        [] => Vec::new(),
        [(start_line, end_line)] => {
            let actual_block_size = end_line - start_line + 1;
            let lines_to_check = (search_block_size - 2).min(actual_block_size - 2);
            let mut similarity = if lines_to_check == 0 { 1.0 } else { 0.0 };
            for index in 1..search_block_size - 1 {
                if index >= actual_block_size - 1 || lines_to_check == 0 {
                    break;
                }
                let original = original_lines[start_line + index].trim();
                let search = search_lines[index].trim();
                let max_len = original.chars().count().max(search.chars().count());
                if max_len == 0 {
                    continue;
                }
                let distance = levenshtein(original, search);
                similarity += (1.0 - distance as f64 / max_len as f64) / lines_to_check as f64;
                if similarity >= SINGLE_CANDIDATE_SIMILARITY_THRESHOLD {
                    break;
                }
            }
            if similarity >= SINGLE_CANDIDATE_SIMILARITY_THRESHOLD {
                vec![line_block(content, &original_lines, *start_line, *end_line)]
            } else {
                Vec::new()
            }
        }
        _ => {
            let mut best_match = None;
            let mut max_similarity = -1.0;
            for (start_line, end_line) in candidates {
                let actual_block_size = end_line - start_line + 1;
                let lines_to_check = (search_block_size - 2).min(actual_block_size - 2);
                let mut similarity = if lines_to_check == 0 { 1.0 } else { 0.0 };
                for index in 1..search_block_size - 1 {
                    if index >= actual_block_size - 1 || lines_to_check == 0 {
                        break;
                    }
                    let original = original_lines[start_line + index].trim();
                    let search = search_lines[index].trim();
                    let max_len = original.chars().count().max(search.chars().count());
                    if max_len == 0 {
                        continue;
                    }
                    let distance = levenshtein(original, search);
                    similarity += 1.0 - distance as f64 / max_len as f64;
                }
                if lines_to_check > 0 {
                    similarity /= lines_to_check as f64;
                }
                if similarity > max_similarity {
                    max_similarity = similarity;
                    best_match = Some((start_line, end_line));
                }
            }
            if max_similarity >= MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD {
                best_match
                    .map(|(start_line, end_line)| {
                        vec![line_block(content, &original_lines, start_line, end_line)]
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
    }
}

fn indentation_flexible(content: &str, find: &str) -> Vec<String> {
    let normalized_find = remove_indentation(find);
    let content_lines: Vec<_> = content.split('\n').collect();
    let find_lines: Vec<_> = find.split('\n').collect();
    if content_lines.len() < find_lines.len() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for start in 0..=content_lines.len() - find_lines.len() {
        let block = content_lines[start..start + find_lines.len()].join("\n");
        if remove_indentation(&block) == normalized_find {
            matches.push(block);
        }
    }
    matches
}

fn escape_normalized(content: &str, find: &str) -> Vec<String> {
    let unescaped_find = unescape_string(find);
    let mut matches = Vec::new();
    if content.contains(&unescaped_find) {
        matches.push(unescaped_find.clone());
    }

    let lines: Vec<_> = content.split('\n').collect();
    let find_lines: Vec<_> = unescaped_find.split('\n').collect();
    if lines.len() < find_lines.len() {
        return matches;
    }
    for start in 0..=lines.len() - find_lines.len() {
        let block = lines[start..start + find_lines.len()].join("\n");
        if unescape_string(&block) == unescaped_find {
            matches.push(block);
        }
    }
    matches
}

fn trimmed_boundary(content: &str, find: &str) -> Vec<String> {
    let trimmed_find = find.trim();
    if trimmed_find == find {
        return Vec::new();
    }

    let mut matches = Vec::new();
    if content.contains(trimmed_find) {
        matches.push(trimmed_find.to_string());
    }

    let lines: Vec<_> = content.split('\n').collect();
    let find_lines: Vec<_> = find.split('\n').collect();
    if lines.len() < find_lines.len() {
        return matches;
    }
    for start in 0..=lines.len() - find_lines.len() {
        let block = lines[start..start + find_lines.len()].join("\n");
        if block.trim() == trimmed_find {
            matches.push(block);
        }
    }
    matches
}

fn context_aware(content: &str, find: &str) -> Vec<String> {
    let mut find_lines: Vec<_> = find.split('\n').collect();
    if find_lines.len() < 3 {
        return Vec::new();
    }
    if find_lines.last().is_some_and(|line| line.is_empty()) {
        find_lines.pop();
    }
    if find_lines.len() < 3 {
        return Vec::new();
    }

    let content_lines: Vec<_> = content.split('\n').collect();
    let first_line = find_lines[0].trim();
    let last_line = find_lines[find_lines.len() - 1].trim();
    let mut matches = Vec::new();

    for start in 0..content_lines.len() {
        if content_lines[start].trim() != first_line {
            continue;
        }
        for end in start.saturating_add(2)..content_lines.len() {
            if content_lines[end].trim() != last_line {
                continue;
            }
            let block_lines = &content_lines[start..=end];
            if block_lines.len() == find_lines.len()
                && context_middle_similarity(block_lines, &find_lines) >= 0.5
            {
                matches.push(block_lines.join("\n"));
                break;
            }
            break;
        }
    }
    matches
}

fn multi_occurrence(content: &str, find: &str) -> Vec<String> {
    let mut matches = Vec::new();
    let mut start = 0;
    while let Some(index) = content[start..].find(find) {
        matches.push(find.to_string());
        start += index + find.len();
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

fn line_block(content: &str, lines: &[&str], start_line: usize, end_line: usize) -> String {
    let match_start = byte_index_for_line(lines, start_line);
    let match_end = byte_index_for_line(lines, end_line + 1) - 1;
    content[match_start..match_end].to_string()
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a.is_empty() || b.is_empty() {
        return a.chars().count().max(b.chars().count());
    }

    let b_chars: Vec<_> = b.chars().collect();
    let mut previous: Vec<_> = (0..=b_chars.len()).collect();
    let mut current = vec![0; b_chars.len() + 1];

    for (i, a_ch) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = usize::from(a_ch != *b_ch);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        previous.clone_from(&current);
    }
    previous[b_chars.len()]
}

fn remove_indentation(text: &str) -> String {
    let lines: Vec<_> = text.split('\n').collect();
    let Some(min_indent) = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading_whitespace_len(line))
        .min()
    else {
        return text.to_string();
    };

    lines
        .into_iter()
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else {
                line[min_indent.min(line.len())..].to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn leading_whitespace_len(line: &str) -> usize {
    let mut len = 0;
    for ch in line.chars() {
        if !ch.is_whitespace() {
            break;
        }
        len += ch.len_utf8();
    }
    len
}

fn unescape_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('`') => out.push('`'),
            Some('\\') => out.push('\\'),
            Some('\n') => out.push('\n'),
            Some('$') => out.push('$'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn context_middle_similarity(block_lines: &[&str], find_lines: &[&str]) -> f64 {
    let mut matching_lines = 0usize;
    let mut total_non_empty_lines = 0usize;

    for index in 1..block_lines.len() - 1 {
        let block_line = block_lines[index].trim();
        let find_line = find_lines[index].trim();
        if block_line.is_empty() && find_line.is_empty() {
            continue;
        }
        total_non_empty_lines += 1;
        if block_line == find_line {
            matching_lines += 1;
        }
    }

    if total_non_empty_lines == 0 {
        1.0
    } else {
        matching_lines as f64 / total_non_empty_lines as f64
    }
}
