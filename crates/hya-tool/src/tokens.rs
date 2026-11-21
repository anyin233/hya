//! Token accounting: how many tokens of the context window a transcript occupies.
//!
//! Two independent concerns live here. [`Tokenizer`] estimates a token count
//! from text alone, for routes that report no usage or report it wrongly.
//! [`TokenAccounting`] decides *which* number to believe for a given route:
//! the provider's reported prompt size, or the local estimate.

use std::sync::Arc;

use hya_proto::context::{
    ReasoningReplayPolicy, ToolImageReplay, current_turn_start, tool_attachment_limit_notice,
    tool_result_text,
};
use hya_proto::{Message, Part, PartId, ToolPartState, ToolSchema};
use serde_json::Value;

/// Counts tokens for a text fragment.
///
/// Implementations are estimators, not encoders: they must be cheap enough to
/// run over a whole transcript on every turn.
pub trait Tokenizer: Send + Sync {
    /// Stable tokenizer name, for diagnostics and events.
    fn name(&self) -> &str;
    /// Estimated token count for `text`.
    fn count_text(&self, text: &str) -> usize;
}

/// Weights in sixteenths, fitted by bounded relative-error least squares
/// against `o200k_base` over repository files plus blob payloads, and quantized
/// so counting is exact integer arithmetic. Refit with
/// `scripts/tokenizer-calibration.py fit`.
mod weight {
    /// An alphanumeric run, which usually encodes as one token up to length 4.
    pub const WORD_RUN: u64 = 15;
    /// Each character beyond the fourth in a run, up to `LONG_RUN`.
    pub const WORD_EXTRA: u64 = 1;
    /// Each character beyond `LONG_RUN` in one run.
    ///
    /// Pinned to the rate measured on high-entropy runs rather than fitted
    /// freely. What a long run truly costs depends on whether the vocabulary
    /// happens to hold its merges — 100 `r`s cost 0.25 per character, 100 `R`s
    /// cost 0.50, concatenated dictionary words 0.08 — and no run-length
    /// feature can tell those apart. Random letters, base64 and hex all measure
    /// 0.50, those are the shapes that arrive in tool output, and the residual
    /// error then falls on the safe side: over-counting compacts a turn early,
    /// under-counting overflows the window.
    pub const WORD_LONG: u64 = 8;
    /// Each digit inside an otherwise alphabetic run.
    pub const DIGIT_CHAR: u64 = 7;
    /// Each change of letter case inside a run, after the first pair.
    pub const CASE_CHANGE: u64 = 5;
    /// A run of nothing but digits, which is a number rather than a name.
    pub const NUMBER_RUN: u64 = 11;
    /// Each digit in such a run.
    pub const NUMBER_CHAR: u64 = 6;
    /// A punctuation, symbol, or multi-space run.
    pub const PUNCT_RUN: u64 = 15;
    /// A newline together with the indentation that follows it.
    pub const NEWLINE_RUN: u64 = 9;
    /// A CJK or other wide scalar value.
    pub const CJK: u64 = 13;
    /// Any remaining non-ASCII scalar value.
    pub const OTHER: u64 = 17;
    /// Common denominator for every weight above.
    pub const SCALE: u64 = 16;
}

/// Lowest scalar value treated as CJK-like for weighting purposes.
const CJK_START: u32 = 0x2E80;

/// Length past which an alphanumeric run stops behaving like a word.
///
/// Up to here BPE folds a run into one or two tokens. Past it the merges run
/// out and the cost settles near half a token per character, which is the
/// regime of base64 attachments, digests and minified bundles arriving through
/// tool output. Charging those the prose rate under-counted them fourfold.
const LONG_RUN: u64 = 12;

// Claude's high-resolution vision tier caps an image at 4,784 tokens, rounded up.
const IMAGE_TOKENS: usize = 5_000;

/// Structure-aware token estimator calibrated against `o200k_base`.
///
/// Classifies text into run categories — alphanumeric names, numbers,
/// punctuation, newline and indentation, CJK — and weights them. On held-out
/// repository corpora it lands within 15% of the true count for 98% of files
/// against 58–84% for the `bytes / 4` heuristic it replaces, and it holds on
/// the two shapes that heuristic gets badly wrong: CJK prose, which `bytes / 4`
/// under-counts by up to a third, and base64-like blobs, which it under-counts
/// by more than half.
#[derive(Clone, Copy, Debug, Default)]
pub struct CalibratedTokenizer;

impl Tokenizer for CalibratedTokenizer {
    fn name(&self) -> &str {
        "calibrated"
    }

    fn count_text(&self, text: &str) -> usize {
        let mut sixteenths: u64 = 0;
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let byte = bytes[i];
            if byte.is_ascii_alphanumeric() {
                // One maximal alphanumeric run, not separate letter and digit
                // runs. BPE merges across the letter/digit boundary inside
                // `sha256`, `utf8` or a base64 blob, so splitting there invents
                // a per-run cost the encoder never charges.
                let start = i;
                let mut digits = 0u64;
                let mut changes = 0u64;
                while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                    digits += u64::from(bytes[i].is_ascii_digit());
                    // Case changes are what separate a base64 blob from a
                    // lowercase hex digest of the same length: alternating case
                    // has few merges in the vocabulary. Counting capitals
                    // instead would charge SCREAMING_SNAKE_CASE, which BPE
                    // merges as happily as prose. The first pair is skipped so
                    // an ordinary initial capital is free.
                    if i - start >= 2
                        && bytes[i].is_ascii_alphabetic()
                        && bytes[i - 1].is_ascii_alphabetic()
                    {
                        changes += u64::from(
                            bytes[i].is_ascii_uppercase() != bytes[i - 1].is_ascii_uppercase(),
                        );
                    }
                    i += 1;
                }
                let run = (i - start) as u64;
                if digits == run {
                    // A bare number. BPE splits these into three-digit groups
                    // at every length, while digits embedded in an identifier
                    // are far cheaper, so the two cannot share one weight.
                    sixteenths += weight::NUMBER_RUN + weight::NUMBER_CHAR * run;
                } else {
                    sixteenths += weight::WORD_RUN
                        + weight::WORD_EXTRA * (run.min(LONG_RUN).saturating_sub(4))
                        + weight::WORD_LONG * run.saturating_sub(LONG_RUN)
                        + weight::DIGIT_CHAR * digits
                        + weight::CASE_CHANGE * changes;
                }
            } else if byte == b'\n' {
                // A newline and the indentation after it collapse into one
                // token in every BPE vocabulary we checked.
                i += 1;
                while i < bytes.len() && matches!(bytes[i], b'\n' | b'\t' | b' ') {
                    i += 1;
                }
                sixteenths += weight::NEWLINE_RUN;
            } else if byte == b' ' || byte == b'\t' {
                let start = i;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                    i += 1;
                }
                let run = (i - start) as u64;
                // A lone separator merges into the neighbouring word token.
                if run > 1 {
                    sixteenths += weight::PUNCT_RUN;
                }
            } else if byte.is_ascii() {
                // Repeated punctuation encodes almost as cheaply as a single
                // character — a rule of 80 dashes is one token — so the run's
                // length carries no weight.
                while i < bytes.len()
                    && bytes[i].is_ascii()
                    && !bytes[i].is_ascii_alphanumeric()
                    && !matches!(bytes[i], b'\n' | b'\t' | b' ')
                {
                    i += 1;
                }
                sixteenths += weight::PUNCT_RUN;
            } else {
                // Non-ASCII: step by whole scalar values so a multi-byte
                // character is never split or counted twice.
                let rest = text.get(i..).unwrap_or_default();
                let Some(character) = rest.chars().next() else {
                    break;
                };
                sixteenths += if character as u32 >= CJK_START {
                    weight::CJK
                } else {
                    weight::OTHER
                };
                i += character.len_utf8();
            }
        }
        // Round rather than truncate: the weights were fitted against whole
        // totals, and truncating loses up to a token on every short fragment.
        let rounded = (sixteenths + weight::SCALE / 2) / weight::SCALE;
        usize::try_from(rounded).unwrap_or(usize::MAX)
    }
}

// The mode and provenance enums are wire types: events, projections, and
// clients all spell them the same way, so hya-proto owns the single definition.
pub use hya_proto::tokens::{TokenAccountingMode, TokenSource};

/// A window-occupancy measurement and how it was derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenCount {
    /// Tokens the transcript is believed to occupy.
    pub tokens: usize,
    /// Provenance of [`Self::tokens`].
    pub source: TokenSource,
}

/// Narrowest ratio of reported-to-estimated tokens still considered plausible.
///
/// Below this the provider is reporting a different quantity (a delta, a
/// post-cache figure, or a different unit) rather than the prompt size.
const PLAUSIBLE_MIN_RATIO: f64 = 0.5;
/// Widest plausible ratio of reported-to-estimated tokens.
const PLAUSIBLE_MAX_RATIO: f64 = 2.0;

/// Decides which token count to believe for a route, and estimates the rest.
#[derive(Clone)]
pub struct TokenAccounting {
    mode: TokenAccountingMode,
    tokenizer: Arc<dyn Tokenizer>,
    sends_tool_images: bool,
}

impl Default for TokenAccounting {
    fn default() -> Self {
        Self::new(TokenAccountingMode::default())
    }
}

impl std::fmt::Debug for TokenAccounting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenAccounting")
            .field("mode", &self.mode)
            .field("tokenizer", &self.tokenizer.name())
            .finish()
    }
}

impl TokenAccounting {
    /// Accounting in `mode` backed by [`CalibratedTokenizer`].
    #[must_use]
    pub fn new(mode: TokenAccountingMode) -> Self {
        Self {
            mode,
            tokenizer: Arc::new(CalibratedTokenizer),
            sends_tool_images: true,
        }
    }

    /// Estimate tool results using the route's image-attachment capability.
    #[must_use]
    pub fn with_tool_images(&self, sends_tool_images: bool) -> Self {
        Self {
            sends_tool_images,
            ..self.clone()
        }
    }

    /// Configured mode.
    #[must_use]
    pub const fn mode(&self) -> TokenAccountingMode {
        self.mode
    }

    /// Backing tokenizer.
    #[must_use]
    pub fn tokenizer(&self) -> &dyn Tokenizer {
        self.tokenizer.as_ref()
    }

    /// Estimated tokens for `messages` as the route's encoder sends them:
    /// reasoning parts count only when `policy` replays them.
    #[must_use]
    pub fn estimate_with_reasoning_policy(
        &self,
        messages: &[Message],
        policy: ReasoningReplayPolicy,
    ) -> usize {
        self.estimate_range(messages, 0..messages.len(), Some(policy))
    }

    /// Estimated tokens for `messages`, ignoring any reported usage and
    /// counting every reasoning part (no route known: over-count, never under).
    #[must_use]
    pub fn estimate(&self, messages: &[Message]) -> usize {
        self.estimate_range(messages, 0..messages.len(), None)
    }

    /// Estimate a composed request from its messages, top-level instructions,
    /// and tool schemas.
    #[must_use]
    pub fn estimate_request(
        &self,
        messages: &[Message],
        system: Option<&str>,
        tools: &[ToolSchema],
        policy: ReasoningReplayPolicy,
    ) -> usize {
        self.estimate_with_reasoning_policy(messages, policy)
            + system.map_or(0, |system| self.tokenizer.count_text(system))
            + tools
                .iter()
                .map(|tool| {
                    self.tokenizer.count_text(tool.name.as_str())
                        + self.tokenizer.count_text(&tool.description)
                        + self.estimate_json(&tool.input_schema)
                })
                .sum::<usize>()
    }

    /// Estimate `messages[range]`. Replay decisions need the whole transcript
    /// (the current turn starts after its last user text), so the window is
    /// computed once over `messages` and indices stay absolute.
    fn estimate_range(
        &self,
        messages: &[Message],
        range: std::ops::Range<usize>,
        policy: Option<ReasoningReplayPolicy>,
    ) -> usize {
        let turn_start = current_turn_start(messages);
        let images = ToolImageReplay::new(messages, self.sends_tool_images);
        let replayed = |index: usize, part: &Part| {
            policy.is_none_or(|policy| policy.replays(turn_start, index, part))
        };
        messages
            .get(range.clone())
            .unwrap_or_default()
            .iter()
            .zip(range)
            .map(|(message, index)| match message {
                Message::User { parts, .. } | Message::Assistant { parts, .. } => parts
                    .iter()
                    .map(|part| match (part, policy) {
                        (Part::Reasoning { .. }, _) if !replayed(index, part) => 0,
                        // Responses-style routes resend only the opaque item.
                        (
                            Part::Reasoning { provider_data, .. },
                            Some(ReasoningReplayPolicy::ProviderData),
                        ) => provider_data
                            .as_ref()
                            .map_or(0, |value| self.estimate_json(value)),
                        _ => self.estimate_part(part, &images),
                    })
                    .sum(),
                Message::System { content, .. } => self.tokenizer.count_text(content),
            })
            .sum()
    }

    /// Best available window occupancy for `messages` on a route that
    /// advertises `usage_reporting`.
    ///
    /// In [`TokenAccountingMode::Auto`] the provider is believed only when it
    /// advertises usage support, actually reported a non-zero figure, and that
    /// figure is plausible against the local estimate. Otherwise the transcript
    /// is estimated outright, which is what makes a route that silently omits
    /// `usage` safe to use.
    #[must_use]
    pub fn tokens_in_use(&self, messages: &[Message], usage_reporting: bool) -> TokenCount {
        self.count_in_use(messages, usage_reporting, None)
    }

    /// [`Self::tokens_in_use`] for a known route: estimates count reasoning
    /// only where the route's encoder replays it.
    #[must_use]
    pub fn tokens_in_use_with_reasoning_policy(
        &self,
        messages: &[Message],
        usage_reporting: bool,
        policy: ReasoningReplayPolicy,
    ) -> TokenCount {
        self.count_in_use(messages, usage_reporting, Some(policy))
    }

    fn count_in_use(
        &self,
        messages: &[Message],
        usage_reporting: bool,
        policy: Option<ReasoningReplayPolicy>,
    ) -> TokenCount {
        let estimated = TokenCount {
            tokens: self.estimate_range(messages, 0..messages.len(), policy),
            source: TokenSource::Estimate,
        };
        if self.mode == TokenAccountingMode::Estimate {
            return estimated;
        }
        let Some((index, reported)) = last_reported_usage(messages) else {
            return estimated;
        };
        // The latest round's prompt plus whatever was appended after it.
        let anchored = TokenCount {
            tokens: reported.saturating_add(self.estimate_range(
                messages,
                index + 1..messages.len(),
                policy,
            )),
            source: TokenSource::Provider,
        };
        if self.mode == TokenAccountingMode::Provider {
            return anchored;
        }
        if !usage_reporting {
            return estimated;
        }
        // Compare like with like: the reported figure describes the prompt up to
        // and including the message that reported it, not the whole transcript.
        let prefix = self.estimate_range(messages, 0..index + 1, policy);
        if prefix == 0 {
            return anchored;
        }
        let ratio = reported as f64 / prefix as f64;
        if (PLAUSIBLE_MIN_RATIO..=PLAUSIBLE_MAX_RATIO).contains(&ratio) {
            anchored
        } else {
            estimated
        }
    }

    /// Estimated tokens for one non-reasoning-filtered part.
    fn estimate_part(&self, part: &Part, images: &ToolImageReplay<'_>) -> usize {
        match part {
            Part::Reasoning {
                text,
                provider_data,
                ..
            } => {
                self.tokenizer.count_text(text)
                    + provider_data
                        .as_ref()
                        .map_or(0, |value| self.estimate_json(value))
            }
            Part::Text { text, .. } => self.tokenizer.count_text(text),
            Part::Media { media_type, .. }
                if media_type
                    .get(..6)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/")) =>
            {
                IMAGE_TOKENS
            }
            Part::Media { data, .. } => data.len() / 4,
            Part::Tool {
                id, name, state, ..
            } => {
                self.tokenizer.count_text(name.as_str())
                    + match state {
                        ToolPartState::Pending { input } | ToolPartState::Running { input } => {
                            self.estimate_json(input)
                        }
                        ToolPartState::Completed { input, output, .. } => {
                            self.estimate_json(input)
                                + self.estimate_tool_output(id, output, images)
                        }
                        ToolPartState::Error {
                            input,
                            message,
                            value,
                            ..
                        } => {
                            self.estimate_json(input)
                                + self.tokenizer.count_text(message)
                                + value.as_ref().map_or(0, |v| self.estimate_json(v))
                        }
                    }
            }
        }
    }

    fn estimate_tool_output(
        &self,
        id: &PartId,
        output: &Value,
        images: &ToolImageReplay<'_>,
    ) -> usize {
        let text = tool_result_text(output);
        let attachments = images
            .attachments(id, output, |_, _| true)
            .map(|(_, _, notice)| match notice {
                Some(notice) => self.tokenizer.count_text(&notice),
                None => IMAGE_TOKENS,
            })
            .sum::<usize>();
        let limit_notice =
            tool_attachment_limit_notice(output).map_or(0, |text| self.tokenizer.count_text(text));
        self.tokenizer.count_text(&text) + attachments + limit_notice
    }

    /// Estimated tokens for a JSON value as the provider will see it.
    fn estimate_json(&self, value: &Value) -> usize {
        match value.as_str() {
            Some(text) => self.tokenizer.count_text(text),
            None => self.tokenizer.count_text(&value.to_string()),
        }
    }
}

/// Index and prompt size of the last message carrying non-zero reported usage.
///
/// Window occupancy is the whole prompt, `input + cache_read + cache_write`:
/// decoders normalize `input` to exclude both cache counters, and cached or
/// cache-written tokens still occupy the window. Legacy logs whose `input`
/// already included cached tokens can only over-count, which fails safe.
fn last_reported_usage(messages: &[Message]) -> Option<(usize, usize)> {
    messages.iter().enumerate().rev().find_map(|(index, m)| {
        let Message::Assistant {
            last_round, tokens, ..
        } = m
        else {
            return None;
        };
        let usage = last_round.as_ref().or(tokens.as_ref())?;
        if usage.is_zero() {
            return None;
        }
        let reported = usize::try_from(usage.prompt()).unwrap_or(usize::MAX);
        Some((index, reported))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_proto::{MessageId, ToolCallId};
    use serde_json::json;

    /// Estimate one message; an assistant message's parts are its last step.
    fn estimate(parts: Vec<Part>, user: bool, sends_tool_images: bool) -> usize {
        if user {
            estimate_message(
                Message::User {
                    id: MessageId::new(),
                    parts,
                },
                sends_tool_images,
            )
        } else {
            estimate_message(assistant(parts, 0), sends_tool_images)
        }
    }

    fn assistant(parts: Vec<Part>, last_step_start: usize) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: "agent".into(),
            model: "model".into(),
            parts,
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: Some(last_step_start),
        }
    }

    fn estimate_message(message: Message, sends_tool_images: bool) -> usize {
        TokenAccounting::default()
            .with_tool_images(sends_tool_images)
            .estimate_request(&[message], None, &[], ReasoningReplayPolicy::None)
    }

    fn tool(output: Value) -> Part {
        Part::Tool {
            id: PartId::new(),
            call_id: ToolCallId::new(),
            name: "example".into(),
            state: ToolPartState::Completed {
                input: json!({}),
                output,
                time_ms: 0,
            },
        }
    }

    fn image_result(url: &str) -> Value {
        json!({
            "output": "Image file.",
            "attachments": [{"type": "file", "mime": "image/png", "url": url}],
        })
    }

    #[test]
    fn request_estimate_charges_images_separately_from_text() {
        let media = |data| {
            vec![Part::Media {
                id: PartId::new(),
                media_type: "image/png".into(),
                data,
                filename: None,
            }]
        };

        let tiny = "data:image/png;base64,AAAA";
        let blob = "AbCD".repeat(128 * 1024);
        let large = format!("data:image/png;base64,{blob}");
        let attached = estimate(vec![tool(image_result(&large))], false, true);
        assert!(
            (IMAGE_TOKENS..IMAGE_TOKENS + 100).contains(&attached),
            "large attachment: {attached}",
        );
        assert_eq!(
            attached,
            estimate(vec![tool(image_result(tiny))], false, true)
        );
        assert_eq!(estimate(media(large.clone()), true, true), IMAGE_TOKENS);
        assert_eq!(estimate(media(tiny.into()), true, true), IMAGE_TOKENS);

        let hidden = estimate(vec![tool(image_result(&large))], false, false);
        assert_eq!(
            hidden,
            estimate(vec![tool(image_result(tiny))], false, false)
        );
        assert!(hidden < 100, "omission notice: {hidden}");

        let fallback = || {
            json!({
                "attachments": [{"type": "file", "mime": "image/png", "url": large}],
            })
        };
        let serialized = estimate(vec![tool(fallback())], false, false);
        assert!(serialized > blob.len() / 4, "JSON fallback: {serialized}");
        assert!(
            (serialized + IMAGE_TOKENS - 100..=serialized + IMAGE_TOKENS).contains(&estimate(
                vec![tool(fallback())],
                false,
                true
            ))
        );

        for output in [json!({"output": blob}), json!({"output": large})] {
            let tokens = estimate(vec![tool(output)], false, true);
            assert!(tokens > blob.len() / 4, "large tool text: {tokens}");
        }

        // The encoder validates the payload. The estimate prices the candidate as an image.
        let mismatched = image_result(&format!("data:image/jpeg;base64,{blob}"));
        assert_eq!(estimate(vec![tool(mismatched)], false, true), attached);
    }

    #[test]
    fn request_estimate_uses_text_for_a_result_with_a_pdf_attachment() {
        let text = "PDF file.";
        let pdf = json!({
            "output": text,
            "attachments": [{
                "type": "file",
                "mime": "application/pdf",
                "url": format!("data:application/pdf;base64,{}", "AbCD".repeat(128 * 1024)),
            }],
        });
        let text_only = json!({"output": text});
        assert_eq!(
            estimate(vec![tool(pdf)], false, true),
            estimate(vec![tool(text_only)], false, true),
        );
    }

    #[test]
    fn request_estimate_counts_an_image_only_in_the_request_after_its_round() {
        let text = estimate(vec![tool(json!({"output": "Image file."}))], false, true);
        let small = "data:image/png;base64,AAAA";
        let image = || tool(image_result(small));
        assert_eq!(estimate(vec![image()], false, true), text + IMAGE_TOKENS);

        // The request after a later round (here the model's text) carries the
        // notice instead.
        let answer = Part::Text {
            id: PartId::new(),
            text: "A board.".into(),
        };
        let not_attached = CalibratedTokenizer.count_text(
            "\n\n[An image is sent to you only in the request right after the call \
             that produced it; later requests, like this one, carry this note \
             instead. This is the normal course, not a failure. To look at the \
             image of this result (image/png), make the call again (for a file: read \
             it again).]",
        );
        assert_eq!(
            estimate_message(assistant(vec![image(), answer], 1), true),
            text + not_attached + CalibratedTokenizer.count_text("A board."),
        );

        // One round's images share a 2 MiB budget; the first is always counted.
        let not_fitted = CalibratedTokenizer.count_text(
            "\n\n[The image attached to this result (image/png) was not shown: this \
             step's images exceed what one request can carry. Read it again in a \
             later step, fewer images at a time.]",
        );
        let fits = 2 * 1024 * 1024 - small.len();
        for (second, expected) in [
            (fits, 2 * text + 2 * IMAGE_TOKENS),
            (fits + 4, 2 * text + IMAGE_TOKENS + not_fitted),
        ] {
            let second = tool(image_result(&"A".repeat(second)));
            assert_eq!(estimate(vec![image(), second], false, true), expected);
        }
        let oversized = tool(image_result(&"A".repeat(3 * 1024 * 1024)));
        assert_eq!(estimate(vec![oversized], false, true), text + IMAGE_TOKENS);
    }
}
