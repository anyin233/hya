use std::path::Path;

use super::path::relative_path;

#[derive(Default)]
pub(super) struct IgnoreSet {
    rules: Vec<Rule>,
}

struct Rule {
    pattern: String,
    directory: bool,
    negated: bool,
    anchored: bool,
}

impl IgnoreSet {
    pub(super) fn load(root: &Path) -> Self {
        let mut rules = Vec::new();
        for name in [".gitignore", ".ignore"] {
            let Ok(text) = std::fs::read_to_string(root.join(name)) else {
                continue;
            };
            for line in text.lines() {
                if let Some(rule) = parse_rule(line) {
                    rules.push(rule);
                }
            }
        }
        Self { rules }
    }

    pub(super) fn matches(&self, root: &Path, path: &Path, is_dir: bool) -> bool {
        let relative = relative_path(root, path);
        let path = if is_dir {
            format!("{relative}/")
        } else {
            relative
        };
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(&path) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

impl Rule {
    fn matches(&self, path: &str) -> bool {
        if self.directory {
            let pattern = self.pattern.trim_end_matches('/');
            if self.anchored || pattern.contains('/') {
                return path.starts_with(&self.pattern);
            }
            return path
                .trim_end_matches('/')
                .split('/')
                .any(|part| pattern_matches(pattern, part));
        }
        if self.anchored {
            return pattern_matches(&self.pattern, path);
        }
        if self.pattern.contains('/') {
            return pattern_matches(&self.pattern, path);
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        pattern_matches(&self.pattern, name)
    }
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    pattern_matches_from(pattern, value, 0, 0, &mut memo)
}

fn pattern_matches_from(
    pattern: &[u8],
    value: &[u8],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][value_index] {
        return result;
    }
    let result = if pattern_index == pattern.len() {
        value_index == value.len()
    } else if pattern.get(pattern_index..pattern_index + 3) == Some(b"**/") {
        pattern_matches_globstar_directory(pattern, value, pattern_index + 3, value_index, memo)
    } else if pattern.get(pattern_index..pattern_index + 2) == Some(b"**") {
        pattern_matches_globstar(pattern, value, pattern_index + 2, value_index, memo)
    } else if pattern[pattern_index] == b'*' {
        pattern_matches_star(pattern, value, pattern_index + 1, value_index, memo)
    } else if value_index == value.len() {
        false
    } else if pattern[pattern_index] == b'?' {
        value[value_index] != b'/'
            && pattern_matches_from(pattern, value, pattern_index + 1, value_index + 1, memo)
    } else if let Some((matched, next_index)) =
        bracket_class_matches(pattern, pattern_index, value[value_index])
    {
        matched
            && value[value_index] != b'/'
            && pattern_matches_from(pattern, value, next_index, value_index + 1, memo)
    } else {
        pattern[pattern_index] == value[value_index]
            && pattern_matches_from(pattern, value, pattern_index + 1, value_index + 1, memo)
    };
    memo[pattern_index][value_index] = Some(result);
    result
}

fn pattern_matches_globstar_directory(
    pattern: &[u8],
    value: &[u8],
    next_pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if pattern_matches_from(pattern, value, next_pattern_index, value_index, memo) {
        return true;
    }
    for index in value_index..value.len() {
        if value[index] == b'/'
            && pattern_matches_from(pattern, value, next_pattern_index, index + 1, memo)
        {
            return true;
        }
    }
    false
}

fn pattern_matches_globstar(
    pattern: &[u8],
    value: &[u8],
    next_pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    for index in value_index..=value.len() {
        if pattern_matches_from(pattern, value, next_pattern_index, index, memo) {
            return true;
        }
    }
    false
}

fn pattern_matches_star(
    pattern: &[u8],
    value: &[u8],
    next_pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    for index in value_index..=value.len() {
        if pattern_matches_from(pattern, value, next_pattern_index, index, memo) {
            return true;
        }
        if value.get(index) == Some(&b'/') {
            break;
        }
    }
    false
}

fn bracket_class_matches(pattern: &[u8], start: usize, value: u8) -> Option<(bool, usize)> {
    if pattern.get(start) != Some(&b'[') || pattern.get(start + 1) == Some(&b']') {
        return None;
    }
    let mut index = start + 1;
    let mut matched = false;
    while index < pattern.len() {
        if pattern[index] == b']' {
            return Some((matched, index + 1));
        }
        if index + 2 < pattern.len() && pattern[index + 1] == b'-' && pattern[index + 2] != b']' {
            let (lower, upper) = if pattern[index] <= pattern[index + 2] {
                (pattern[index], pattern[index + 2])
            } else {
                (pattern[index + 2], pattern[index])
            };
            matched |= lower <= value && value <= upper;
            index += 3;
        } else {
            matched |= pattern[index] == value;
            index += 1;
        }
    }
    None
}

fn parse_rule(line: &str) -> Option<Rule> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (negated, trimmed) = trimmed
        .strip_prefix('!')
        .map_or((false, trimmed), |pattern| (true, pattern.trim()));
    if trimmed.is_empty() {
        return None;
    }
    let anchored = trimmed.starts_with('/');
    let directory = trimmed.ends_with('/');
    let pattern = trimmed.trim_start_matches('/').to_string();
    Some(Rule {
        pattern,
        directory,
        negated,
        anchored,
    })
}
