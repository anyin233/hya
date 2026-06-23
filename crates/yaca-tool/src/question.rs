use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::interaction::{QuestionAnswer, QuestionKind};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) struct QuestionTool;

#[derive(Deserialize)]
struct QuestionToolInput {
    questions: Vec<QuestionInput>,
}

#[derive(Deserialize)]
struct QuestionInput {
    question: String,
    #[serde(default, rename = "header")]
    _header: String,
    #[serde(default)]
    options: Vec<QuestionOptionInput>,
    #[serde(default)]
    custom: Option<bool>,
    #[serde(default, rename = "multiple")]
    _multiple: bool,
}

#[derive(Clone, Deserialize)]
struct QuestionOptionInput {
    label: String,
    #[serde(rename = "description")]
    _description: String,
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
        let mut answers = Vec::with_capacity(input.questions.len());
        for question in &input.questions {
            answers.push(ask_question(ctx, question).await);
        }

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

async fn ask_question(ctx: &ToolCtx, question: &QuestionInput) -> Vec<String> {
    let labels = question
        .options
        .iter()
        .map(|option| option.label.clone())
        .collect::<Vec<_>>();
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

    match ctx.interaction.ask(question.question.clone(), kind).await {
        Ok(QuestionAnswer::Selected(index)) => labels
            .get(index)
            .cloned()
            .map_or_else(Vec::new, |label| vec![label]),
        Ok(QuestionAnswer::SelectedMany(indices)) => indices
            .into_iter()
            .filter_map(|index| labels.get(index).cloned())
            .collect(),
        Ok(QuestionAnswer::FreeText(text)) if text.is_empty() => Vec::new(),
        Ok(QuestionAnswer::FreeText(text)) => vec![text],
        Ok(QuestionAnswer::Cancelled) | Err(_) => Vec::new(),
    }
}
