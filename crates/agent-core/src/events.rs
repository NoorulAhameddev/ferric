use crate::models::TokenUsage;
use serde::{Deserialize, Serialize};

/// Normalized streaming event model recommended and approved by the architecture review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStarted {
        message_id: String,
    },
    ThinkingDelta {
        text: String,
    },
    TextDelta {
        text: String,
    },
    ToolCallStarted {
        index: u32,
        id: Option<String>,
        name: Option<String>,
    },
    ToolCallDelta {
        index: u32,
        arguments_chunk: String,
    },
    ToolCallCompleted {
        index: u32,
    },
    StatusUpdate {
        status: String,
    },
    MessageCompleted {
        usage: Option<TokenUsage>,
    },
    StreamError {
        error: String,
    },
}

impl StreamEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StreamEvent::MessageCompleted { .. } | StreamEvent::StreamError { .. }
        )
    }
}
