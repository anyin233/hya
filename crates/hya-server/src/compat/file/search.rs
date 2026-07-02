use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use super::path::relative_path;

pub(super) fn ranked_paths<I>(root: &Path, paths: I, query: &str) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    let query = query.to_ascii_lowercase();
    let mut matches = paths
        .into_iter()
        .filter_map(|path| {
            let relative = relative_path(root, &path).to_ascii_lowercase();
            score(&relative, &query).map(|score| (score, relative, path))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left_path, _), (right_score, right_path, _)| {
        left_score
            .cmp(right_score)
            .then_with(|| left_path.cmp(right_path))
    });
    matches.into_iter().map(|(_, _, path)| path).collect()
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Score {
    tier: usize,
    gap: usize,
    start: usize,
    len: usize,
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            .then_with(|| self.gap.cmp(&other.gap))
            .then_with(|| self.start.cmp(&other.start))
            .then_with(|| self.len.cmp(&other.len))
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn score(candidate: &str, query: &str) -> Option<Score> {
    if query.is_empty() {
        return Some(Score {
            tier: 0,
            gap: 0,
            start: 0,
            len: candidate.len(),
        });
    }
    if let Some(start) = candidate.find(query) {
        return Some(Score {
            tier: 0,
            gap: 0,
            start,
            len: candidate.len(),
        });
    }
    fuzzy_score(candidate, query)
}

fn fuzzy_score(candidate: &str, query: &str) -> Option<Score> {
    let mut query_chars = query.chars();
    let mut needed = query_chars.next()?;
    let mut first = None;
    let mut previous = None;
    let mut gap = 0usize;

    for (index, candidate_char) in candidate.chars().enumerate() {
        if candidate_char != needed {
            continue;
        }
        if let Some(previous) = previous {
            gap += index.saturating_sub(previous + 1);
        }
        first.get_or_insert(index);
        previous = Some(index);
        match query_chars.next() {
            Some(next) => needed = next,
            None => {
                return Some(Score {
                    tier: 1,
                    gap,
                    start: first.unwrap_or(index),
                    len: candidate.len(),
                });
            }
        }
    }
    None
}
