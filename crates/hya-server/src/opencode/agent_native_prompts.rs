const PROMPT_EXPLORE: &str = include_str!("agent_prompts/explore.txt");
const PROMPT_COMPACTION: &str = include_str!("agent_prompts/compaction.txt");
const PROMPT_TITLE: &str = include_str!("agent_prompts/title.txt");
const PROMPT_SUMMARY: &str = include_str!("agent_prompts/summary.txt");

pub(super) fn get(name: &str) -> Option<&'static str> {
    match name {
        "explore" => Some(PROMPT_EXPLORE.trim_end()),
        "compaction" => Some(PROMPT_COMPACTION.trim_end()),
        "title" => Some(PROMPT_TITLE.trim_end()),
        "summary" => Some(PROMPT_SUMMARY.trim_end()),
        _ => None,
    }
}
