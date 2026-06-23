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
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star_index, mut star_value_index) = (None, 0);
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(index) = star_index {
            pattern_index = index + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
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
