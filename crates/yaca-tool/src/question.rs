use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::interaction::{
    QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption, QuestionPrompt,
};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) struct QuestionTool;

#[derive(Deserialize)]
struct QuestionToolInput {
    questions: Vec<QuestionInput>,
}

#[derive(Deserialize)]
struct QuestionInput {
    question: String,
    header: String,
    #[serde(default)]
    options: Vec<QuestionOptionInput>,
    #[serde(default)]
    custom: Option<bool>,
    #[serde(default, rename = "multiple")]
    multiple: bool,
}

#[derive(Clone, Deserialize)]
struct QuestionOptionInput {
    label: String,
    description: String,
}

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        "question"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("question"),
            description: "Ask the user one or more questions during execution.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
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
                                "custom": { "type": "boolean" }
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
        let input: QuestionToolInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let prompts = input
            .questions
            .iter()
            .map(question_prompt)
            .collect::<Vec<_>>();
        let mut raw_answers = ctx
            .interaction
            .ask_many(prompts)
            .await
            .unwrap_or_else(|_| Vec::new())
            .into_iter();
        let answers = input
            .questions
            .iter()
            .map(|question| {
                raw_answers
                    .next()
                    .map_or_else(Vec::new, |answer| answer_labels(question, answer))
            })
            .collect::<Vec<_>>();

        let formatted = input
            .questions
            .iter()
            .zip(&answers)
            .map(|(question, answer)| {
                let answer = if answer.is_empty() {
                    "Unanswered".to_string()
                } else {
                    answer.join(", ")
                };
                format!("\"{}\"=\"{answer}\"", question.question)
            })
            .collect::<Vec<_>>()
            .join(", ");

        Ok(json!({
            "title": format!(
                "Asked {} question{}",
                input.questions.len(),
                if input.questions.len() > 1 { "s" } else { "" }
            ),
            "output": format!(
                "User has answered your questions: {formatted}. You can now continue with the user's answers in mind."
            ),
            "metadata": { "answers": answers },
        }))
    }
}

fn question_prompt(question: &QuestionInput) -> QuestionPrompt {
    let labels = labels(question);
    let kind = if labels.is_empty() {
        QuestionKind::FreeText {
            default: Some(String::new()),
        }
    } else {
        QuestionKind::Select {
            options: labels.clone(),
            allow_custom: question.custom.unwrap_or(true),
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
        custom: question.custom,
    };
    QuestionPrompt::new(info, kind)
}

fn answer_labels(question: &QuestionInput, answer: QuestionAnswer) -> Vec<String> {
    let labels = labels(question);
    match answer {
        QuestionAnswer::Selected(index) => labels
            .get(index)
            .cloned()
            .map_or_else(Vec::new, |label| vec![label]),
        QuestionAnswer::SelectedMany(indices) => indices
            .into_iter()
            .filter_map(|index| labels.get(index).cloned())
            .collect(),
        QuestionAnswer::FreeText(text) if text.is_empty() => Vec::new(),
        QuestionAnswer::FreeText(text) => vec![text],
        QuestionAnswer::Cancelled => Vec::new(),
    }
}

fn labels(question: &QuestionInput) -> Vec<String> {
    question
        .options
        .iter()
        .map(|option| option.label.clone())
        .collect()
}
