//! Where a fold cuts the transcript, and the summary a fold anchors on.

use hya_proto::context::{COMPACT_CONTEXT_MARKER, RESPONSES_COMPACT_ITEMS_MARKER};
use hya_proto::{Message, MessageId, Part, PartId, StepCut};

/// What one fold takes out of the request, and what it records.
#[derive(Clone, Debug)]
pub(crate) struct FoldCut {
    /// What the summarizer reads: the folded messages, then for a step cut the
    /// kept turn head (as context) and the in-flight message cut to its
    /// folded steps.
    pub(crate) folded: Vec<Message>,
    /// First message folded.
    pub(crate) from_message: MessageId,
    /// Last message folded (the in-flight message for a step cut).
    pub(crate) to_message: MessageId,
    /// Messages folded whole, plus one for a step cut.
    pub(crate) folded_count: u32,
    /// Where the fold cut inside the in-flight message.
    pub(crate) step_cut: Option<StepCut>,
}

/// Plan the next fold of `messages`, which start after the latest compaction
/// marker or at the session start.
///
/// The message cut folds everything before the last `keep_messages`. When it
/// folds nothing, or what it keeps is still over `threshold`, and the last
/// message is an in-flight assistant with more than one step, the fold cuts
/// inside that message instead: whole messages up to the turn's head, plus all
/// but the newest `keep_steps` steps (fewer while the kept turn is over half
/// the threshold, never fewer than one). A step is cut whole, reasoning, calls
/// and results together, so every kept step goes out exactly as stored.
/// Calls no reasoning leads may be cut apart.
///
/// `count` estimates a transcript as the route will be sent it.
pub(crate) fn fold_cut(
    messages: &[Message],
    keep_messages: usize,
    keep_steps: usize,
    threshold: usize,
    count: &dyn Fn(&[Message]) -> usize,
) -> Option<FoldCut> {
    let start = messages
        .iter()
        .rposition(|message| {
            matches!(
                message,
                Message::System { content, .. }
                    if content.starts_with(COMPACT_CONTEXT_MARKER)
            )
        })
        .map_or(0, |index| index + 1);
    let message_cut = messages
        .len()
        .checked_sub(keep_messages)
        .filter(|end| start < *end);
    if let Some(end) = message_cut
        && count(&messages[end..]) <= threshold
    {
        return Some(whole_messages(messages, start, end));
    }
    step_fold(messages, start, keep_steps, threshold, count)
        .or_else(|| message_cut.map(|end| whole_messages(messages, start, end)))
}

fn whole_messages(messages: &[Message], start: usize, end: usize) -> FoldCut {
    FoldCut {
        folded: messages[start..end].to_vec(),
        from_message: messages[start].id(),
        to_message: messages[end - 1].id(),
        folded_count: u32::try_from(end - start).unwrap_or(u32::MAX),
        step_cut: None,
    }
}

/// The step cut of [`fold_cut`]; `None` unless the last message is an
/// assistant holding more than one step.
fn step_fold(
    messages: &[Message],
    start: usize,
    keep_steps: usize,
    threshold: usize,
    count: &dyn Fn(&[Message]) -> usize,
) -> Option<FoldCut> {
    let last = messages
        .len()
        .checked_sub(1)
        .filter(|last| start <= *last)?;
    let Message::Assistant { id, parts, .. } = &messages[last] else {
        return None;
    };
    // A step starts where a non-tool part follows the previous step's tool
    // calls. Between two calls a cut is also safe when no reasoning leads
    // them (a model that calls tools without thinking aloud); a call led by
    // reasoning stays with it even when text sits between the reasoning and
    // the calls. Every call keeps its result: a tool part holds both.
    let mut step_starts = Vec::new();
    let mut led_by_reasoning = matches!(parts.first(), Some(Part::Reasoning { .. }));
    for at in 1..parts.len() {
        let after_call = matches!(parts[at - 1], Part::Tool { .. });
        let is_call = matches!(parts[at], Part::Tool { .. });
        if after_call && (!is_call || !led_by_reasoning) {
            step_starts.push(at);
            led_by_reasoning = matches!(parts[at], Part::Reasoning { .. });
        } else if matches!(parts[at], Part::Reasoning { .. }) {
            led_by_reasoning = true;
        }
    }
    // The turn's head: the prompt and notices right before the message.
    let head = messages[start..last]
        .iter()
        .rposition(|message| matches!(message, Message::Assistant { .. }))
        .map_or(start, |at| start + at + 1);
    let kept_turn = |cut: usize| {
        let mut kept = messages[head..last].to_vec();
        kept.push(with_parts(&messages[last], parts[cut..].to_vec()));
        kept
    };
    let steps = step_starts.len() + 1;
    let cut_keeping = |kept: usize| {
        if kept == steps {
            0
        } else {
            step_starts[steps - kept - 1]
        }
    };
    let mut kept = keep_steps.clamp(1, steps);
    while kept > 1 && count(&kept_turn(cut_keeping(kept))) > threshold / 2 {
        kept -= 1;
    }
    if kept == steps {
        return None;
    }
    let cut = cut_keeping(kept);
    let mut folded = messages[start..last].to_vec();
    folded.push(with_parts(&messages[last], parts[..cut].to_vec()));
    let through_part = part_id(&parts[cut - 1]);
    let kept_head = if head < last {
        messages[head].id()
    } else {
        *id
    };
    let from_message = if start < head {
        messages[start].id()
    } else {
        *id
    };
    Some(FoldCut {
        folded,
        from_message,
        to_message: *id,
        folded_count: u32::try_from(head - start + 1).unwrap_or(u32::MAX),
        step_cut: Some(StepCut {
            through_part,
            kept_head,
        }),
    })
}

/// `message` (an assistant) with `parts` in place of its own.
fn with_parts(message: &Message, parts: Vec<Part>) -> Message {
    let mut message = message.clone();
    if let Message::Assistant { parts: own, .. } = &mut message {
        *own = parts;
    }
    message
}

fn part_id(part: &Part) -> PartId {
    match part {
        Part::Text { id, .. }
        | Part::Reasoning { id, .. }
        | Part::Media { id, .. }
        | Part::Tool { id, .. } => *id,
    }
}

/// The anchored summary already present in `messages`, if any.
///
/// Compaction is incremental: the summary a session already carries is the
/// baseline the next one updates. Re-summarizing a summary from scratch is how
/// detail decays across a long session, because each pass paraphrases the
/// previous pass's prose rather than the original events.
///
/// Returns `None` for a window folded by the provider's own compaction, whose
/// body is serialized response items rather than prose and means nothing to a
/// summarizer prompt.
pub(crate) fn previous_summary(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        let Message::System { content, .. } = m else {
            return None;
        };
        let rest = content.strip_prefix(COMPACT_CONTEXT_MARKER)?.trim_start();
        if rest.starts_with(RESPONSES_COMPACT_ITEMS_MARKER) {
            return None;
        }
        let rest = hya_proto::context::without_marker_goal(rest);
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests {
    use super::*;
    use hya_proto::{ModelRef, ToolPartState};

    pub(crate) fn user(text: &str) -> Message {
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: text.to_string(),
            }],
        }
    }

    /// Counts a transcript as the token accounting does with no route known.
    fn estimate(messages: &[Message]) -> usize {
        hya_tool::tokens::TokenAccounting::default().estimate(messages)
    }

    fn reasoning(text: &str) -> Part {
        Part::Reasoning {
            id: PartId::new(),
            text: text.to_string(),
            provider_data: Some(serde_json::json!({
                "type": "reasoning",
                "encrypted_content": format!("{text} {}", "lorem ipsum ".repeat(500)),
            })),
        }
    }

    fn tool_step(step: &str) -> Part {
        use hya_proto::{ToolCallId, ToolName};
        Part::Tool {
            id: PartId::new(),
            call_id: ToolCallId::new(),
            name: ToolName::new("find"),
            state: ToolPartState::Completed {
                input: serde_json::json!({ "step": step }),
                output: serde_json::json!("ok"),
                time_ms: 1,
            },
        }
    }

    fn assistant(parts: Vec<Part>) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts,
            finish: None,
            tokens: None,
            last_round: None,
        }
    }

    fn notice(text: &str) -> Message {
        Message::System {
            id: MessageId::new(),
            content: text.to_string(),
        }
    }

    fn parts_of(message: &Message) -> &[Part] {
        match message {
            Message::Assistant { parts, .. } | Message::User { parts, .. } => parts,
            Message::System { .. } => &[],
        }
    }

    /// `[R0 T0] [R1 T1a T1b] [X2 R2 T2] [R3 T3]`: reasoning first, parallel
    /// calls, and a step that opens with text.
    fn four_steps() -> Vec<Part> {
        vec![
            reasoning("r0"),
            tool_step("t0"),
            reasoning("r1"),
            tool_step("t1a"),
            tool_step("t1b"),
            Part::Text {
                id: PartId::new(),
                text: "x2".to_string(),
            },
            reasoning("r2"),
            tool_step("t2"),
            reasoning("r3"),
            tool_step("t3"),
        ]
    }

    #[test]
    fn message_fold_reports_the_exact_folded_range() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cut = fold_cut(&msgs, 2, 2, 1_000, &estimate).expect("a foldable prefix");
        // Folds the prefix before the retained recent messages: 6 - 2 = 4.
        assert_eq!(cut.folded_count, 4);
        assert_eq!(cut.from_message, msgs[0].id());
        assert_eq!(cut.to_message, msgs[3].id());
        assert_eq!(cut.step_cut, None);
        assert_eq!(cut.folded, msgs[..4]);
    }

    #[test]
    fn fold_cut_is_none_without_a_foldable_range() {
        assert!(fold_cut(&[user("short")], 2, 2, 1_000, &estimate).is_none());
    }

    /// The single-turn shape: whole steps fold, the prompt and the newest
    /// steps stay, and a cut never parts reasoning from the calls it led to or
    /// a parallel call from its sibling.
    #[test]
    fn step_cut_falls_between_whole_steps() {
        let msgs = vec![user("go"), assistant(four_steps())];
        let parts = parts_of(&msgs[1]);
        for (keep_steps, cut) in [(1, 8), (2, 5), (3, 2)] {
            let fold = fold_cut(&msgs, 2, keep_steps, usize::MAX, &estimate)
                .expect("an in-flight turn with several steps folds");
            assert_eq!(fold.from_message, msgs[1].id(), "no whole message folds");
            assert_eq!(fold.to_message, msgs[1].id());
            assert_eq!(fold.folded_count, 1);
            assert_eq!(
                fold.step_cut,
                Some(StepCut {
                    through_part: part_id(&parts[cut - 1]),
                    kept_head: msgs[0].id(),
                }),
                "keeping {keep_steps} steps"
            );
            assert_eq!(fold.folded.len(), 2, "the prompt, then the folded steps");
            assert_eq!(fold.folded[0], msgs[0]);
            assert_eq!(parts_of(&fold.folded[1]), &parts[..cut]);
        }
    }

    /// A model that calls tools without reasoning or text between them still
    /// has steps to fold; calls led by reasoning are never cut from it.
    #[test]
    fn tool_calls_without_reasoning_are_steps_of_their_own() {
        let calls = vec![
            user("go"),
            assistant((0..3).map(|n| tool_step(&n.to_string())).collect()),
        ];
        let parts = parts_of(&calls[1]);
        let fold = fold_cut(&calls, 2, 1, usize::MAX, &estimate).expect("fold");
        assert_eq!(
            fold.step_cut.map(|cut| cut.through_part),
            Some(part_id(&parts[1]))
        );

        let led = vec![
            user("go"),
            assistant(vec![
                reasoning("r"),
                tool_step("a"),
                tool_step("b"),
                tool_step("c"),
            ]),
        ];
        assert!(
            fold_cut(&led, 2, 1, usize::MAX, &estimate).is_none(),
            "calls led by reasoning are one step"
        );

        let led_with_text = vec![
            user("go"),
            assistant(vec![
                reasoning("r"),
                Part::Text {
                    id: PartId::new(),
                    text: "checking both".to_string(),
                },
                tool_step("a"),
                tool_step("b"),
            ]),
        ];
        assert!(
            fold_cut(&led_with_text, 2, 1, usize::MAX, &estimate).is_none(),
            "text between reasoning and parallel calls does not split the step"
        );
    }

    #[test]
    fn steps_that_fit_the_kept_window_are_not_cut() {
        let msgs = vec![user("go"), assistant(four_steps())];
        assert!(fold_cut(&msgs, 7, 4, usize::MAX, &estimate).is_none());
        assert!(fold_cut(&msgs, 7, 6, usize::MAX, &estimate).is_none());
        let one_step = vec![user("go"), assistant(vec![reasoning("r"), tool_step("t")])];
        assert!(fold_cut(&one_step, 7, 1, 0, &estimate).is_none());
    }

    /// Few large steps: the kept turn shrinks below half the threshold, but
    /// at least the newest step stays.
    #[test]
    fn kept_steps_shrink_to_half_the_threshold() {
        let msgs = vec![user("go"), assistant(four_steps())];
        let step = estimate(&[assistant(vec![reasoning("r3"), tool_step("t3")])]);
        let fold = fold_cut(&msgs, 7, 6, 3 * step, &estimate).expect("over half: fold");
        let parts = parts_of(&msgs[1]);
        assert_eq!(
            fold.step_cut.map(|cut| cut.through_part),
            Some(part_id(&parts[7])),
            "only the newest step fits under half the threshold"
        );
        let fold = fold_cut(&msgs, 7, 6, 0, &estimate).expect("never below one step");
        assert_eq!(
            fold.step_cut.map(|cut| cut.through_part),
            Some(part_id(&parts[7]))
        );
    }

    /// A continuation turn: the earlier turn's message folds whole, the new
    /// turn's notice stays as its head.
    #[test]
    fn continuation_turn_folds_the_earlier_message_and_keeps_its_notice() {
        let msgs = vec![
            user("go"),
            notice("HEADLESS RUN — time budget"),
            assistant(vec![reasoning("a"), tool_step("a")]),
            notice("HEADLESS RUN — continue"),
            assistant(four_steps()),
        ];
        let fold = fold_cut(&msgs, 7, 1, usize::MAX, &estimate).expect("fold");
        assert_eq!(fold.from_message, msgs[0].id());
        assert_eq!(fold.to_message, msgs[4].id());
        assert_eq!(fold.folded_count, 4, "three whole messages and the cut one");
        assert_eq!(
            fold.step_cut.map(|cut| cut.kept_head),
            Some(msgs[3].id()),
            "the continuation notice stays"
        );
        assert_eq!(&fold.folded[..4], &msgs[..4]);
        assert_eq!(parts_of(&fold.folded[4]).len(), 8);
    }

    /// After a step fold the transcript starts at its marker; the next fold
    /// keeps the same head and cuts further into the same message.
    #[test]
    fn a_later_step_fold_starts_after_the_marker() {
        let marker = notice(&format!("{}\nearlier steps", COMPACT_CONTEXT_MARKER));
        let msgs = vec![marker, user("go"), assistant(four_steps())];
        let fold = fold_cut(&msgs, 2, 1, usize::MAX, &estimate).expect("fold");
        assert_eq!(fold.from_message, msgs[2].id());
        assert_eq!(fold.folded_count, 1);
        assert_eq!(fold.step_cut.map(|cut| cut.kept_head), Some(msgs[1].id()));
        assert_eq!(fold.folded[0], msgs[1], "the summarizer sees the prompt");
    }

    /// An interactive session whose message fold keeps a tail under the
    /// threshold folds whole messages exactly as before, even mid-turn.
    #[test]
    fn a_message_cut_that_fits_is_preferred() {
        let msgs = vec![
            user("first"),
            assistant(vec![Part::Text {
                id: PartId::new(),
                text: "answer".to_string(),
            }]),
            user("second"),
            assistant(four_steps()),
        ];
        let fold = fold_cut(&msgs, 2, 1, usize::MAX, &estimate).expect("fold");
        assert_eq!(fold.step_cut, None);
        assert_eq!(fold.from_message, msgs[0].id());
        assert_eq!(fold.to_message, msgs[1].id());
        assert_eq!(fold.folded, msgs[..2]);
        // The same cut whose kept tail is over the threshold cuts the turn.
        let fold = fold_cut(&msgs, 2, 1, estimate(&msgs[2..]) - 1, &estimate).expect("fold");
        assert_eq!(fold.to_message, msgs[3].id());
        assert_eq!(fold.step_cut.map(|cut| cut.kept_head), Some(msgs[2].id()));
    }
    #[test]
    fn previous_summary_reads_the_anchored_marker() {
        let msgs = vec![
            Message::System {
                id: MessageId::new(),
                content: format!("{}\nEarlier: shipped the parser.", COMPACT_CONTEXT_MARKER),
            },
            user("continue"),
        ];
        assert_eq!(
            previous_summary(&msgs).as_deref(),
            Some("Earlier: shipped the parser.")
        );
    }

    #[test]
    fn previous_summary_ignores_unmarked_and_provider_windows() {
        let plain = vec![Message::System {
            id: MessageId::new(),
            content: "Summary of earlier conversation:\nstuff".to_string(),
        }];
        assert_eq!(
            previous_summary(&plain),
            None,
            "an unmarked system message is not an anchored summary"
        );

        let provider_window = vec![Message::System {
            id: MessageId::new(),
            content: format!(
                "{}\n{}\n[]",
                COMPACT_CONTEXT_MARKER, RESPONSES_COMPACT_ITEMS_MARKER
            ),
        }];
        assert_eq!(
            previous_summary(&provider_window),
            None,
            "a provider-folded window is serialized response items, not prose"
        );
    }
}
