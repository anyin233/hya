use std::path::Path;

use super::path::relative_path;

#[derive(Default)]
pub(super) struct IgnoreSet {
    rules: Vec<Rule>,
}

struct Rule {
    pattern: String,
    directory: bool,
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
        self.rules.iter().any(|rule| rule.matches(&path))
    }
}

impl Rule {
    fn matches(&self, path: &str) -> bool {
        if self.directory {
            return path.starts_with(&self.pattern);
        }
        if self.pattern.contains('/') {
            return pattern_matches(&self.pattern, path);
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        pattern_matches(&self.pattern, name)
    }
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');
    let mut rest = value;
    for (index, part) in pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        let Some(offset) = rest.find(part) else {
            return false;
        };
        if index == 0 && !starts_with_wildcard && offset != 0 {
            return false;
        }
        rest = &rest[offset + part.len()..];
    }
    ends_with_wildcard || rest.is_empty()
}

fn parse_rule(line: &str) -> Option<Rule> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
        return None;
    }
    let directory = trimmed.ends_with('/');
    let pattern = trimmed.trim_start_matches('/').to_string();
    Some(Rule { pattern, directory })
}
