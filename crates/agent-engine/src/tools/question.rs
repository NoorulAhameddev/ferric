use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
}

#[async_trait]
pub trait QuestionHandler: Send + Sync {
    async fn ask_question(
        &self,
        question: &str,
        options: &[QuestionOption],
        allow_custom: bool,
    ) -> Result<String, String>;
}

#[async_trait]
impl<F> QuestionHandler for F
where
    F: Fn(&str, &[QuestionOption], bool) -> Result<String, String> + Send + Sync,
{
    async fn ask_question(
        &self,
        question: &str,
        options: &[QuestionOption],
        allow_custom: bool,
    ) -> Result<String, String> {
        self(question, options, allow_custom)
    }
}

static QUESTION_HANDLER: RwLock<Option<Arc<dyn QuestionHandler>>> = RwLock::new(None);

pub fn set_question_handler(handler: Option<Arc<dyn QuestionHandler>>) {
    let mut lock = QUESTION_HANDLER.write().unwrap();
    *lock = handler;
}

pub fn clear_question_handler() {
    set_question_handler(None);
}

pub struct AskUserQuestionTool;

impl Default for AskUserQuestionTool {
    fn default() -> Self {
        Self
    }
}

impl AskUserQuestionTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "AskUserQuestion".into(),
            description: "Prompt the user with an interactive question along with selectable option chips and optional custom response."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to present to the user"
                    },
                    "options": {
                        "type": "array",
                        "description": "Selectable option chips presented to the user",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Display label for the option chip" },
                                "value": { "type": "string", "description": "Return value when selected" },
                                "description": { "type": "string", "description": "Optional details about this option" }
                            },
                            "required": ["label"]
                        }
                    },
                    "allow_custom": {
                        "type": "boolean",
                        "description": "Whether to permit freeform user response (default: true)"
                    }
                },
                "required": ["question"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let question = input["question"]
            .as_str()
            .ok_or("Missing 'question' argument")?;
        let allow_custom = input["allow_custom"].as_bool().unwrap_or(true);

        let mut options = Vec::new();
        if let Some(arr) = input["options"].as_array() {
            for (idx, opt_val) in arr.iter().enumerate() {
                if let Some(label_str) = opt_val.as_str() {
                    options.push(QuestionOption {
                        label: label_str.to_string(),
                        value: label_str.to_string(),
                        description: None,
                    });
                } else if let Some(obj) = opt_val.as_object() {
                    let label = obj
                        .get("label")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("Option at index {idx} missing 'label'"))?
                        .to_string();
                    let value = obj
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| label.clone());
                    let description = obj
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    options.push(QuestionOption {
                        label,
                        value,
                        description,
                    });
                }
            }
        }

        let handler_opt = {
            let lock = QUESTION_HANDLER.read().unwrap();
            lock.clone()
        };

        if let Some(handler) = handler_opt {
            handler.ask_question(question, &options, allow_custom).await
        } else {
            let mut out = format!("Question: {question}\n");
            if !options.is_empty() {
                out.push_str("Options:\n");
                for (idx, opt) in options.iter().enumerate() {
                    let desc = opt
                        .description
                        .as_deref()
                        .map(|d| format!(" - {d}"))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "{}. [{}] (value: '{}'){}\n",
                        idx + 1,
                        opt.label,
                        opt.value,
                        desc
                    ));
                }
            }
            if allow_custom {
                out.push_str("(Custom responses are allowed)\n");
            }
            out.push_str("(Interactive prompt rendered for user selection)");
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ask_user_question_default_format() {
        set_question_handler(None);
        let tool = AskUserQuestionTool;
        let dummy_root = Path::new(".");

        let input = json!({
            "question": "Which database would you like to use?",
            "options": [
                { "label": "PostgreSQL", "value": "postgres", "description": "Production SQL DB" },
                { "label": "SQLite", "value": "sqlite", "description": "Local embedded DB" }
            ],
            "allow_custom": false
        });

        let res = tool.execute(input, dummy_root).await.unwrap();
        assert!(res.contains("Which database would you like to use?"));
        assert!(res.contains("1. [PostgreSQL] (value: 'postgres') - Production SQL DB"));
        assert!(res.contains("2. [SQLite] (value: 'sqlite') - Local embedded DB"));
    }

    #[tokio::test]
    async fn test_ask_user_question_with_handler() {
        let handler = |question: &str, options: &[QuestionOption], _allow_custom: bool| {
            assert_eq!(question, "Select package manager");
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].value, "pnpm");
            Ok("Selected: pnpm".to_string())
        };

        set_question_handler(Some(Arc::new(handler)));
        let tool = AskUserQuestionTool;
        let dummy_root = Path::new(".");
        let input = json!({
            "question": "Select package manager",
            "options": ["pnpm", "npm"]
        });

        let res = tool.execute(input, dummy_root).await.unwrap();
        assert_eq!(res, "Selected: pnpm");

        set_question_handler(None);
    }
}
