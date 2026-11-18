//! The `shake` step: stale tool-output bodies leave the request. With the
//! session's artifact store as the sink, a body moves to disk and the
//! transcript keeps a handle that reads it back.

use std::path::Path;

use hya_proto::context::TODO_TOOL;
use hya_proto::{EvictedPart, Message, Part, ToolPartState};
use hya_tool::handle::{ArtifactPlane, ArtifactStore};

/// Replaces a dropped tool output when no sink preserved the body.
///
/// This is the lossy path: the bytes are gone from the request and re-running
/// the tool is the model's only recourse, which costs what it cost the first
/// time. Taken only when no sink is wired.
const EVICTED_OUTPUT_NOTICE: &str = "[tool output evicted to fit the context window; \
                                     re-run the tool if you still need it]";

/// Opening of the notice left when a body was preserved rather than dropped.
///
/// Matched as a prefix so a later pass recognizes its own work.
const SPILLED_OUTPUT_PREFIX: &str = "[tool output moved to ";

/// Where an evicted tool-output body is preserved so the model can fetch it back.
///
/// With a sink, eviction is a *move* rather than a drop: the body lands in
/// durable storage and the transcript keeps a handle that retrieves it. Without
/// one the output is simply gone, which is the difference between the cheapest
/// reduction being recoverable and being merely cheap.
pub(crate) trait EvictionSink: Send + Sync {
    /// Persist `body`, produced by `tool`, and return the handle that reads it back.
    ///
    /// Returning `None` falls back to the lossy notice. A sink that cannot store
    /// one particular body must not fail the compaction that is trying to keep
    /// the turn inside its window.
    fn spill(&self, tool: &str, body: &str) -> Option<String>;
}

/// Notice left in place of an output whose body moved to `handle`.
fn spilled_output_notice(handle: &str) -> String {
    format!(
        "{SPILLED_OUTPUT_PREFIX}{handle} to fit the context window; \
         read that handle to retrieve it]"
    )
}

/// Whether `output` is a notice a previous eviction pass already left behind.
///
/// Both shapes are recognized so a repeat pass is a no-op and the reported
/// count reflects real work rather than re-evicting a notice.
fn is_eviction_notice(output: &serde_json::Value) -> bool {
    // Prefixes: a notice may carry an appended mail notice.
    output.as_str().is_some_and(|text| {
        text.starts_with(EVICTED_OUTPUT_NOTICE) || text.starts_with(SPILLED_OUTPUT_PREFIX)
    })
}

/// Completed, not-yet-evicted tool outputs that are stale: outside the last
/// `keep_messages` messages (older turns, as before) OR outside the last
/// `keep_steps` tool steps. The step tail is what lets a long turn — one
/// assistant message holding many tool steps — shed its own early outputs.
fn stale_tool_outputs(
    messages: &[Message],
    keep_messages: usize,
    keep_steps: usize,
) -> Vec<(usize, usize)> {
    let mut candidates = Vec::new();
    let mut latest_todo = None;
    for (message_index, message) in messages.iter().enumerate() {
        let parts = match message {
            Message::Assistant { parts, .. } | Message::User { parts, .. } => parts,
            Message::System { .. } => continue,
        };
        for (part_index, part) in parts.iter().enumerate() {
            if let Part::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } = part
                && !is_eviction_notice(output)
            {
                let location = (message_index, part_index);
                if name.as_str() == TODO_TOOL {
                    latest_todo = Some(location);
                }
                candidates.push(location);
            }
        }
    }
    let step_cut = candidates.len().saturating_sub(keep_steps);
    let message_cut = messages.len().saturating_sub(keep_messages);
    candidates
        .into_iter()
        .enumerate()
        .filter(|(rank, (message_index, _))| *rank < step_cut || *message_index < message_cut)
        .map(|(_, location)| location)
        .filter(|location| Some(*location) != latest_todo)
        .collect()
}

/// Replace stale tool outputs in `messages` with notices, spilling each body
/// to `sink` when one is wired, and return the parts it evicted with their
/// notices for the `ContextEvicted` record.
///
/// A notice keeps an appended `--- [NEW MAIL ...]` steer notice verbatim, so
/// consumed mail stays visible to the model.
pub(crate) fn evict_stale_tool_outputs(
    messages: &mut [Message],
    keep_messages: usize,
    keep_steps: usize,
    sink: Option<&dyn EvictionSink>,
) -> Vec<EvictedPart> {
    let mut evicted = Vec::new();
    for (message_index, part_index) in stale_tool_outputs(messages, keep_messages, keep_steps) {
        let (Message::Assistant { parts, .. } | Message::User { parts, .. }) =
            &mut messages[message_index]
        else {
            continue;
        };
        let Part::Tool {
            id,
            name,
            state: ToolPartState::Completed { output, .. },
            ..
        } = &mut parts[part_index]
        else {
            continue;
        };
        let body = value_text(output);
        let mail = body
            .find("--- [NEW MAIL")
            .map(|index| body[index..].to_string());
        let mut notice = sink
            .and_then(|sink| sink.spill(name.as_str(), &body))
            .map_or_else(
                || EVICTED_OUTPUT_NOTICE.to_string(),
                |handle| spilled_output_notice(&handle),
            );
        if let Some(mail) = mail {
            notice.push('\n');
            notice.push_str(&mail);
        }
        *output = serde_json::Value::String(notice.clone());
        evicted.push(EvictedPart { part: *id, notice });
    }
    evicted
}

/// Whether at least one completed tool output is stale enough to spill.
pub(crate) fn has_spillable_tool_output(
    messages: &[Message],
    keep_messages: usize,
    keep_steps: usize,
) -> bool {
    !stale_tool_outputs(messages, keep_messages, keep_steps).is_empty()
}

/// Render a JSON value as the model would have seen it.
fn value_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToString::to_string)
}

/// Smallest body worth moving to disk: the notice left behind is around a
/// hundred characters, so a smaller body takes the lossy notice instead.
const MIN_SPILL_BYTES: usize = 512;

/// Preserves evicted tool output in one session's artifact store.
pub(crate) struct ArtifactEvictionSink {
    store: ArtifactStore,
}

impl ArtifactEvictionSink {
    /// Spill into the artifact directory of the session rooted at `workdir`,
    /// the root the `read` tool resolves `artifact://` handles against.
    pub(crate) fn new(workdir: &Path) -> Self {
        Self {
            store: ArtifactPlane::default().store(workdir),
        }
    }
}

impl EvictionSink for ArtifactEvictionSink {
    fn spill(&self, tool: &str, body: &str) -> Option<String> {
        if body.len() < MIN_SPILL_BYTES {
            return None;
        }
        // A spill that cannot reach disk degrades to the lossy notice: this
        // rung keeps the request inside its window, and a full disk must not
        // stand in its way.
        self.store
            .store(tool, "text/plain", body.as_bytes())
            .ok()
            .map(|meta| format!("artifact://{}", meta.id))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::{MessageId, ModelRef, PartId};

    fn assistant_with_tool(output: &str) -> Message {
        use hya_proto::{ToolCallId, ToolName, ToolPartState};
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: ToolCallId::new(),
                name: ToolName::new("find"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({"pattern": "*.rs"}),
                    output: serde_json::Value::String(output.to_string()),
                    time_ms: 7,
                },
            }],
            finish: None,
            tokens: None,
            last_round: None,
        }
    }

    fn tool_output_of(message: &Message) -> Option<String> {
        let Message::Assistant { parts, .. } = message else {
            return None;
        };
        parts.iter().find_map(|p| match p {
            Part::Tool {
                state: ToolPartState::Completed { output, .. },
                ..
            } => output.as_str().map(ToString::to_string),
            _ => None,
        })
    }

    fn evict(messages: &mut [Message], keep: usize, sink: Option<&dyn EvictionSink>) -> usize {
        evict_stale_tool_outputs(messages, keep, keep, sink).len()
    }

    #[test]
    fn eviction_keeps_only_the_newest_todo_result_when_both_are_stale() {
        let mut older = assistant_with_tool("Current todos:\n- #1 [pending] first");
        let mut latest = assistant_with_tool("Current todos:\n- #1 [completed] first");
        for message in [&mut older, &mut latest] {
            let Message::Assistant { parts, .. } = message else {
                panic!("expected assistant tool message");
            };
            let Part::Tool { name, .. } = &mut parts[0] else {
                panic!("expected tool part");
            };
            *name = hya_proto::ToolName::new(TODO_TOOL);
        }
        let mut messages = [older, latest, assistant_with_tool("recent")];
        assert_eq!(evict(&mut messages, 1, None), 1);
        assert_eq!(
            tool_output_of(&messages[0]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
        assert_eq!(
            tool_output_of(&messages[1]).as_deref(),
            Some("Current todos:\n- #1 [completed] first")
        );
    }

    #[test]
    fn eviction_drops_stale_outputs_keeps_inputs_and_respects_keep_recent() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD_OUTPUT_A".repeat(100)),
            assistant_with_tool(&"OLD_OUTPUT_B".repeat(100)),
            assistant_with_tool(&"RECENT_OUTPUT".repeat(100)),
        ];
        let accounting = hya_tool::tokens::TokenAccounting::default();
        let before = accounting.estimate(&msgs);

        let evicted = evict(&mut msgs, 1, None);
        assert_eq!(evicted, 2, "only the two stale messages are evicted");
        assert!(
            accounting.estimate(&msgs) < before,
            "eviction must reduce the token count"
        );

        // Stale outputs replaced by the notice; the recent one is untouched.
        assert_eq!(
            tool_output_of(&msgs[0]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
        assert_eq!(
            tool_output_of(&msgs[1]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
        assert!(
            tool_output_of(&msgs[2]).is_some_and(|o| o.contains("RECENT_OUTPUT")),
            "the most recent tool output must survive"
        );

        // The call and its input survive, so the model still knows what it ran.
        let Message::Assistant { parts, .. } = &msgs[0] else {
            panic!("expected assistant");
        };
        let Part::Tool { name, state, .. } = &parts[0] else {
            panic!("expected tool part");
        };
        assert_eq!(name.as_str(), "find");
        let ToolPartState::Completed { input, .. } = state else {
            panic!("expected completed state");
        };
        assert_eq!(input["pattern"], "*.rs", "tool input must be preserved");
    }
    #[test]
    fn eviction_spills_old_tool_steps_inside_active_assistant() {
        use hya_proto::{ToolCallId, ToolName, ToolPartState};
        let parts = (0..4)
            .map(|index| Part::Tool {
                id: PartId::new(),
                call_id: ToolCallId::new(),
                name: ToolName::new("find"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({"step": index}),
                    output: serde_json::Value::String(format!(
                        "output-{index}-{}",
                        "x".repeat(100)
                    )),
                    time_ms: 1,
                },
            })
            .collect();
        let mut messages = vec![Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts,
            finish: None,
            tokens: None,
            last_round: None,
        }];

        assert_eq!(evict(&mut messages, 2, None), 2);
        let Message::Assistant { parts, .. } = &messages[0] else {
            panic!("expected assistant");
        };
        let outputs: Vec<_> = parts
            .iter()
            .map(|part| match part {
                Part::Tool {
                    state: ToolPartState::Completed { output, .. },
                    ..
                } => output.as_str().unwrap_or_default().to_string(),
                _ => String::new(),
            })
            .collect();
        assert!(outputs[0].starts_with("[tool output evicted"));
        assert!(outputs[1].starts_with("[tool output evicted"));
        assert!(outputs[2].starts_with("output-2"));
        assert!(outputs[3].starts_with("output-3"));
    }

    #[test]
    fn eviction_is_idempotent() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];
        assert_eq!(evict(&mut msgs, 1, None), 1);
        assert_eq!(
            evict(&mut msgs, 1, None),
            0,
            "a second pass has nothing left to evict"
        );
    }

    /// Records what it was handed, and hands back a deterministic handle.
    struct RecordingSink(std::sync::Mutex<Vec<(String, String)>>);

    impl RecordingSink {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.0.lock().expect("sink lock").clone()
        }
    }

    impl EvictionSink for RecordingSink {
        fn spill(&self, tool: &str, body: &str) -> Option<String> {
            let mut calls = self.0.lock().expect("sink lock");
            calls.push((tool.to_string(), body.to_string()));
            Some(format!("artifact://spill-{}", calls.len()))
        }
    }

    /// With a sink the body is preserved and the transcript says where it went,
    /// which is the difference between evicting output and losing it.
    #[test]
    fn eviction_with_a_sink_leaves_a_retrievable_handle() {
        let body = "OLD_OUTPUT_A".repeat(100);
        let mut msgs = vec![assistant_with_tool(&body), assistant_with_tool("recent")];
        let sink = RecordingSink::new();

        assert_eq!(evict(&mut msgs, 1, Some(&sink)), 1);

        let calls = sink.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0, "find",
            "the sink is told which tool produced it"
        );
        assert_eq!(
            calls[0].1, body,
            "the whole body is preserved, not a preview"
        );
        let notice = tool_output_of(&msgs[0]).expect("evicted output carries a notice");
        assert!(
            notice.contains("artifact://spill-1"),
            "the transcript must name the handle that retrieves the body: {notice}"
        );
    }

    /// A spilled notice is recognized as already-evicted, so a later pass does
    /// not spill the notice itself and count that as work.
    #[test]
    fn eviction_is_idempotent_over_spilled_notices() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];
        let sink = RecordingSink::new();

        assert_eq!(evict(&mut msgs, 1, Some(&sink)), 1);
        assert_eq!(
            evict(&mut msgs, 1, Some(&sink)),
            0,
            "a second pass has nothing left to evict"
        );
        assert_eq!(sink.calls().len(), 1, "the notice must not be re-spilled");
    }
    /// A notice keeps the mail appended to the output, and a later pass
    /// recognizes such a notice as its own work.
    #[test]
    fn eviction_preserves_mail_notice_and_skips_it_on_a_later_pass() {
        let body = format!(
            "{}\n--- [NEW MAIL from scout] child failed",
            "OLD".repeat(200)
        );
        let mut msgs = vec![assistant_with_tool(&body), assistant_with_tool("recent")];

        let evicted = evict_stale_tool_outputs(&mut msgs, 1, 1, None);
        assert_eq!(evicted.len(), 1);
        let notice = tool_output_of(&msgs[0]).expect("placeholder");
        assert_eq!(evicted[0].notice, notice);
        assert!(notice.contains("--- [NEW MAIL from scout] child failed"));
        assert_eq!(evict(&mut msgs, 1, None), 0);
    }

    /// A sink that declines degrades to the lossy notice rather than failing the
    /// reduction that is trying to keep the turn inside its window.
    #[test]
    fn eviction_falls_back_to_the_lossy_notice_when_a_sink_declines() {
        struct Declines;
        impl EvictionSink for Declines {
            fn spill(&self, _tool: &str, _body: &str) -> Option<String> {
                None
            }
        }
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];

        assert_eq!(evict(&mut msgs, 1, Some(&Declines)), 1);
        assert_eq!(
            tool_output_of(&msgs[0]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
    }

    /// A body below the floor costs more to reference than to keep.
    #[test]
    fn small_bodies_are_not_spilled() {
        let dir = std::env::temp_dir().join(format!("hya-spill-small-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sink = ArtifactEvictionSink::new(&dir);

        assert!(sink.spill("bash", "short output").is_none());
        assert!(
            !dir.join(".hya/tool-output").exists(),
            "a refused spill must not create the artifact directory"
        );
    }

    /// A spilled body is addressable by the handle the notice carries.
    #[test]
    fn spilled_body_round_trips_through_its_handle() {
        let dir = std::env::temp_dir().join(format!("hya-spill-round-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = "x".repeat(MIN_SPILL_BYTES);
        let sink = ArtifactEvictionSink::new(&dir);

        let handle = sink.spill("bash", &body).unwrap();

        let id = handle.strip_prefix("artifact://").unwrap();
        let stored = std::fs::read_to_string(dir.join(".hya/tool-output").join(id)).unwrap();
        assert_eq!(stored, body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
