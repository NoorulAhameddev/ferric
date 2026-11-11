use agent_core::{ContentBlock, Message, Role};

pub const OLD_TOOL_RESULT_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";
pub const DEFAULT_KEEP_RECENT_TURNS: usize = 2;
pub const MIN_CONTENT_LEN_FOR_MICROCOMPACT: usize = 150;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrocompactResult {
    pub messages_modified: usize,
    pub bytes_reclaimed: usize,
}

/// Performs lightweight microcompaction on historical conversation turns.
/// Clears bulky tool result payloads from turns older than `keep_recent_turns`
/// while preserving the turn structure, role hierarchy, and tool call IDs.
pub fn microcompact_messages(
    messages: &mut [Message],
    keep_recent_turns: usize,
) -> MicrocompactResult {
    let mut modified = 0;
    let mut reclaimed = 0;

    let len = messages.len();
    if len <= keep_recent_turns {
        return MicrocompactResult {
            messages_modified: 0,
            bytes_reclaimed: 0,
        };
    }

    let compact_cutoff = len.saturating_sub(keep_recent_turns);

    for msg in &mut messages[..compact_cutoff] {
        let mut msg_changed = false;
        for block in &mut msg.content {
            if let ContentBlock::ToolResult { content, .. } = block {
                if content.len() > MIN_CONTENT_LEN_FOR_MICROCOMPACT
                    && content != OLD_TOOL_RESULT_CLEARED_MESSAGE
                {
                    reclaimed += content
                        .len()
                        .saturating_sub(OLD_TOOL_RESULT_CLEARED_MESSAGE.len());
                    *content = OLD_TOOL_RESULT_CLEARED_MESSAGE.to_string();
                    msg_changed = true;
                }
            }
        }
        if msg_changed {
            modified += 1;
        }
    }

    MicrocompactResult {
        messages_modified: modified,
        bytes_reclaimed: reclaimed,
    }
}

/// Summarizes older turns in a conversation history when the context exceeds
/// `max_history_turns`, replacing them with a synthesized summary block while
/// preserving the most recent `keep_recent_turns`.
pub fn compact_conversation(
    messages: &mut Vec<Message>,
    max_history_turns: usize,
    keep_recent_turns: usize,
) -> bool {
    if messages.len() <= max_history_turns {
        return false;
    }

    let split_idx = messages.len().saturating_sub(keep_recent_turns);
    if split_idx == 0 {
        return false;
    }

    // Extract older messages to summarize
    let older_messages: Vec<Message> = messages.drain(..split_idx).collect();

    let mut summary_lines = Vec::new();
    summary_lines.push(format!(
        "[Compressed History: {} earlier messages summarized for context efficiency]",
        older_messages.len()
    ));

    for (idx, msg) in older_messages.iter().enumerate() {
        let role_str = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };

        let mut text_preview = String::new();
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    let first_line = text.lines().next().unwrap_or("").trim();
                    if !first_line.is_empty() {
                        text_preview.push_str(first_line);
                    }
                }
                ContentBlock::ToolUse { name, .. } => {
                    text_preview.push_str(&format!("[ToolUse: {name}] "));
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    text_preview.push_str(&format!("[ToolResult: {tool_use_id}] "));
                }
            }
        }

        if !text_preview.is_empty() {
            let truncated = if text_preview.len() > 120 {
                format!("{}...", &text_preview[..120])
            } else {
                text_preview
            };
            summary_lines.push(format!("Turn {}: {}: {}", idx + 1, role_str, truncated));
        }
    }

    let summary_message = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: summary_lines.join("\n"),
        }],
    };

    messages.insert(0, summary_message);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microcompact_messages_clears_older_bulky_tools() {
        let mut messages = vec![
            // Turn 1 (Old)
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Read big file".into(),
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "FileRead".into(),
                        input: serde_json::json!({ "file_path": "big.txt" }),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "A".repeat(500),
                        is_error: false,
                    },
                ],
            },
            // Turn 2 (Old)
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_2".into(),
                    content: "Short result".into(), // < 150 bytes, should not be cleared
                    is_error: false,
                }],
            },
            // Turn 3 (Recent - Keep)
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_3".into(),
                    content: "B".repeat(500),
                    is_error: false,
                }],
            },
        ];

        let result = microcompact_messages(&mut messages, 1);
        assert_eq!(result.messages_modified, 1);
        assert!(result.bytes_reclaimed > 400);

        // Turn 1 ToolResult cleared
        if let ContentBlock::ToolResult { content, .. } = &messages[1].content[1] {
            assert_eq!(content, OLD_TOOL_RESULT_CLEARED_MESSAGE);
        } else {
            panic!("Expected ToolResult");
        }

        // Turn 2 ToolResult untouched because small
        if let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] {
            assert_eq!(content, "Short result");
        } else {
            panic!("Expected ToolResult");
        }

        // Turn 3 ToolResult untouched because recent
        if let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] {
            assert_eq!(content.len(), 500);
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn test_compact_conversation_summarizes_history() {
        let mut messages = Vec::new();
        for i in 1..=10 {
            messages.push(Message::user_text(format!("Prompt {i}")));
            messages.push(Message::assistant_text(format!("Response {i}")));
        }
        assert_eq!(messages.len(), 20);

        // Compact when > 6 messages, keeping recent 4
        let compacted = compact_conversation(&mut messages, 6, 4);
        assert!(compacted);
        assert_eq!(messages.len(), 5); // 1 summary + 4 recent

        if let ContentBlock::Text { text } = &messages[0].content[0] {
            assert!(text.contains("[Compressed History:"));
            assert!(text.contains("Prompt 1"));
        } else {
            panic!("Expected summary text block");
        }
    }
}
