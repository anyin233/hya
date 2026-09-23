//! Merged `ask_user` tool: batch structured questions over the
//! [`crate::interaction::InteractionPlane`], with structured per-question
//! answers and explicit cancellation. The legacy `question` spelling stays
//! dispatchable as a hidden registry alias (same batch input shape).

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use hya_tool::{QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption, QuestionPrompt};
use hya_tool::{Tool, ToolCtx, ToolError};

pub(crate) struct AskUserTool;

#[derive(Deserialize)]
struct AskUserInput {
    questions: Vec<QuestionInput>,
}

#[derive(Deserialize)]
struct QuestionInput {
    question: String,
    header: String,
    #[serde(default)]
    options: Vec<QuestionOptionInput>,
    #[serde(default)]
    multiple: bool,
    /// Whether a custom write-in answer is accepted for select questions.
    /// Legacy `question` calls may spell this `custom`.
    #[serde(default, alias = "custom")]
    allow_custom: Option<bool>,
    /// Default free-text answer (used when no options are given).
    #[serde(default)]
    default: Option<String>,
}

#[derive(Clone, Deserialize)]
struct QuestionOptionInput {
    label: String,
    description: String,
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("ask_user"),
            description: "Ask the user one or more questions and wait for their answers. Each question needs a short header; give options with label+description for a choice (multiple for multi-select, allow_custom to permit write-ins), or omit options for free text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string" },
                                "header": { "type": "string" },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" }
                                        },
                                        "required": ["label", "description"]
                                    }
                                },
                                "multiple": { "type": "boolean" },
                                "allow_custom": { "type": "boolean" },
                                "default": { "type": "string" }
                            },
                            "required": ["question", "header", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: AskUserInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let prompts = input
            .questions
            .iter()
            .map(question_prompt)
            .collect::<Vec<_>>();
        let raw_answers = ctx
            .interaction
            .ask_many(prompts)
            .await
            .map_err(|e| ToolError::Other(format!("ask_user unavailable: {e}")))?;
        let answers = input
            .questions
            .iter()
            .zip(raw_answers)
            .map(|(question, answer)| answer_entry(question, answer))
            .collect::<Vec<_>>();

        let formatted = answers
            .iter()
            .map(|entry| {
                let answer = if entry.answer.is_empty() {
                    "Unanswered".to_string()
                } else {
                    entry.answer.join(", ")
                };
                format!("\"{}\"=\"{answer}\"", entry.question)
            })
            .collect::<Vec<_>>()
            .join(", ");

        let metadata_answers: Vec<Value> = answers
            .iter()
            .map(|entry| {
                json!({
                    "question": entry.question,
                    "answer": entry.answer,
                    "cancelled": entry.cancelled,
                })
            })
            .collect();

        Ok(json!({
            "title": format!(
                "Asked {} question{}",
                answers.len(),
                if answers.len() > 1 { "s" } else { "" }
            ),
            "output": format!(
                "User has answered your questions: {formatted}. You can now continue with the user's answers in mind."
            ),
            "metadata": { "answers": metadata_answers },
        }))
    }
}

struct AnswerEntry {
    question: String,
    answer: Vec<String>,
    cancelled: bool,
}

fn answer_entry(question: &QuestionInput, answer: QuestionAnswer) -> AnswerEntry {
    let labels = option_labels(question);
    let (answer, cancelled) = match answer {
        QuestionAnswer::Selected(index) => (
            labels
                .get(index)
                .cloned()
                .map_or_else(Vec::new, |label| vec![label]),
            false,
        ),
        QuestionAnswer::SelectedMany(indices) => (
            indices
                .into_iter()
                .filter_map(|index| labels.get(index).cloned())
                .collect(),
            false,
        ),
        QuestionAnswer::FreeText(text) if text.is_empty() => (Vec::new(), true),
        QuestionAnswer::FreeText(text) => (vec![text], false),
        QuestionAnswer::Cancelled => (Vec::new(), true),
    };
    AnswerEntry {
        question: question.question.clone(),
        answer,
        cancelled,
    }
}

fn question_prompt(question: &QuestionInput) -> QuestionPrompt {
    let labels = option_labels(question);
    let kind = if labels.is_empty() {
        QuestionKind::FreeText {
            default: question.default.clone(),
        }
    } else {
        QuestionKind::Select {
            options: labels.clone(),
            allow_custom: question.allow_custom.unwrap_or(true),
        }
    };
    let info = QuestionInfo {
        question: question.question.clone(),
        header: question.header.clone(),
        options: question
            .options
            .iter()
            .map(|option| QuestionOption {
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect(),
        multiple: question.multiple,
        custom: question.allow_custom,
    };
    QuestionPrompt::new(info, kind)
}

fn option_labels(question: &QuestionInput) -> Vec<String> {
    question
        .options
        .iter()
        .map(|option| option.label.clone())
        .collect()
}
