use crate::error::CoreError;
use crate::events::StreamEvent;
use crate::models::{ContentBlock, Message, Role, TokenUsage};
use serde_json::Value;
use std::collections::HashMap;

const DEFAULT_MAX_ARG_BUFFER_BYTES: usize = 10 * 1024 * 1024; // 10MB limit

#[derive(Debug, Default, Clone)]
struct InFlightToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments_buffer: String,
    completed: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallAccumulator {
    message_id: Option<String>,
    text_buffer: String,
    thinking_buffer: String,
    tool_calls: HashMap<u32, InFlightToolCall>,
    usage: Option<TokenUsage>,
    max_buffer_bytes: usize,
}

impl Default for ToolCallAccumulator {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ARG_BUFFER_BYTES)
    }
}

impl ToolCallAccumulator {
    pub fn new(max_buffer_bytes: usize) -> Self {
        Self {
            message_id: None,
            text_buffer: String::new(),
            thinking_buffer: String::new(),
            tool_calls: HashMap::new(),
            usage: None,
            max_buffer_bytes,
        }
    }

    /// Access the accumulated thinking text, if any.
    pub fn thinking(&self) -> Option<&str> {
        if self.thinking_buffer.is_empty() {
            None
        } else {
            Some(&self.thinking_buffer)
        }
    }

    /// Process a streaming event and update internal buffers.
    pub fn process_event(&mut self, event: &StreamEvent) -> Result<(), CoreError> {
        match event {
            StreamEvent::MessageStarted { message_id } => {
                self.message_id = Some(message_id.clone());
            }
            StreamEvent::ThinkingDelta { text } => {
                self.thinking_buffer.push_str(text);
            }
            StreamEvent::TextDelta { text } => {
                self.text_buffer.push_str(text);
            }
            StreamEvent::StatusUpdate { .. } => {}
            StreamEvent::ToolCallStarted { index, id, name } => {
                let entry = self.tool_calls.entry(*index).or_default();
                if let Some(id_val) = id {
                    entry.id = Some(id_val.clone());
                }
                if let Some(name_val) = name {
                    entry.name = Some(name_val.clone());
                }
            }
            StreamEvent::ToolCallDelta {
                index,
                arguments_chunk,
            } => {
                let entry = self.tool_calls.entry(*index).or_default();
                if entry.arguments_buffer.len() + arguments_chunk.len() > self.max_buffer_bytes {
                    return Err(CoreError::Accumulator(format!(
                        "Tool call index {} exceeded maximum argument buffer limit of {} bytes",
                        index, self.max_buffer_bytes
                    )));
                }
                entry.arguments_buffer.push_str(arguments_chunk);
            }
            StreamEvent::ToolCallCompleted { index } => {
                if let Some(entry) = self.tool_calls.get_mut(index) {
                    entry.completed = true;
                }
            }
            StreamEvent::MessageCompleted { usage } => {
                if usage.is_some() {
                    self.usage = usage.clone();
                }
            }
            StreamEvent::StreamError { error } => {
                return Err(CoreError::Accumulator(format!(
                    "Stream error received: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Converts accumulated buffers into a final validated Assistant `Message`.
    /// Validates all tool argument JSON strings upon completion.
    pub fn finalize(mut self) -> Result<(Message, Option<TokenUsage>), CoreError> {
        let mut content = Vec::new();

        if !self.text_buffer.is_empty() {
            content.push(ContentBlock::Text {
                text: self.text_buffer,
            });
        }

        // Sort tool calls by index to preserve deterministic order
        let mut indices: Vec<u32> = self.tool_calls.keys().copied().collect();
        indices.sort_unstable();

        for index in indices {
            let tc = self.tool_calls.remove(&index).unwrap();
            let name = tc.name.unwrap_or_else(|| format!("tool_{index}"));
            let id = tc.id.unwrap_or_else(|| format!("call_{index}"));

            // Parse raw JSON arguments. Fallback to empty object if empty string.
            let input: Value = if tc.arguments_buffer.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&tc.arguments_buffer).map_err(|e| {
                    CoreError::ToolValidation(format!(
                        "Failed to parse tool call '{}' arguments JSON: {}. Raw: {}",
                        name, e, tc.arguments_buffer
                    ))
                })?
            };

            content.push(ContentBlock::ToolUse { id, name, input });
        }

        Ok((
            Message {
                role: Role::Assistant,
                content,
            },
            self.usage,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_accumulation() {
        let mut acc = ToolCallAccumulator::default();
        acc.process_event(&StreamEvent::MessageStarted {
            message_id: "msg_1".into(),
        })
        .unwrap();
        acc.process_event(&StreamEvent::TextDelta {
            text: "Hello, ".into(),
        })
        .unwrap();
        acc.process_event(&StreamEvent::TextDelta {
            text: "world!".into(),
        })
        .unwrap();
        acc.process_event(&StreamEvent::MessageCompleted { usage: None })
            .unwrap();

        let (msg, _) = acc.finalize().unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(
            msg.content,
            vec![ContentBlock::Text {
                text: "Hello, world!".into()
            }]
        );
    }

    #[test]
    fn test_tool_call_accumulation_and_json_parse() {
        let mut acc = ToolCallAccumulator::default();
        acc.process_event(&StreamEvent::ToolCallStarted {
            index: 0,
            id: Some("call_abc123".into()),
            name: Some("FileRead".into()),
        })
        .unwrap();
        acc.process_event(&StreamEvent::ToolCallDelta {
            index: 0,
            arguments_chunk: "{\"file_".into(),
        })
        .unwrap();
        acc.process_event(&StreamEvent::ToolCallDelta {
            index: 0,
            arguments_chunk: "path\": \"src/main.rs\"}".into(),
        })
        .unwrap();
        acc.process_event(&StreamEvent::ToolCallCompleted { index: 0 })
            .unwrap();

        let (msg, _) = acc.finalize().unwrap();
        assert_eq!(msg.content.len(), 1);
        if let ContentBlock::ToolUse { id, name, input } = &msg.content[0] {
            assert_eq!(id, "call_abc123");
            assert_eq!(name, "FileRead");
            assert_eq!(input["file_path"], "src/main.rs");
        } else {
            panic!("Expected ContentBlock::ToolUse");
        }
    }

    #[test]
    fn test_usage_preservation_on_none() {
        let mut acc = ToolCallAccumulator::default();
        let initial_usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        };
        acc.process_event(&StreamEvent::MessageCompleted {
            usage: Some(initial_usage.clone()),
        })
        .unwrap();
        // A subsequent MessageCompleted event with None must not overwrite existing usage
        acc.process_event(&StreamEvent::MessageCompleted { usage: None })
            .unwrap();

        let (_, usage) = acc.finalize().unwrap();
        assert_eq!(usage, Some(initial_usage));
    }
}
