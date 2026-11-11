use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tool_calling: bool,
    pub structured_output: bool,
    pub vision: bool,
    pub max_context_tokens: usize,
    pub max_output_tokens: usize,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            tool_calling: true,
            structured_output: true,
            vision: false,
            max_context_tokens: 200_000,
            max_output_tokens: 4_096,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CachePolicy {
    Disabled,
    Prefer,
    Require,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderExtensions {
    pub entries: BTreeMap<String, serde_json::Value>,
}

impl ProviderExtensions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, provider_id: impl Into<String>, value: serde_json::Value) {
        self.entries.insert(provider_id.into(), value);
    }

    pub fn get(&self, provider_id: &str) -> Option<&serde_json::Value> {
        self.entries.get(provider_id)
    }

    /// Check if a header name is a protected transport-level header that extensions must never override.
    pub fn is_reserved_header(header: &str) -> bool {
        let h = header.to_ascii_lowercase();
        matches!(
            h.as_str(),
            "authorization"
                | "host"
                | "content-length"
                | "content-type"
                | "connection"
                | "transfer-encoding"
                | "cookie"
                | "set-cookie"
        )
    }

    /// Safely retrieve namespaced extra headers for a specific provider, rejecting any reserved headers.
    pub fn safe_headers_for(&self, provider_id: &str) -> HashMap<String, String> {
        let mut result = HashMap::new();
        if let Some(val) = self.get(provider_id) {
            if let Some(headers) = val.get("headers").and_then(|h| h.as_object()) {
                for (k, v) in headers {
                    if !Self::is_reserved_header(k) {
                        if let Some(s) = v.as_str() {
                            result.insert(k.clone(), s.to_string());
                        }
                    } else {
                        tracing::warn!(
                            "Blocked attempt to set reserved transport header '{}' via provider extensions for '{}'",
                            k,
                            provider_id
                        );
                    }
                }
            }
        }
        result
    }
}

/// Normalized provider-neutral completion request (ChatGPT Option C architecture).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub generation: GenerationConfig,
    #[serde(default)]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub cache_policy: Option<CachePolicy>,
    #[serde(default)]
    pub extensions: ProviderExtensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub message_id: String,
    pub message: Message,
    pub usage: Option<TokenUsage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_extensions_header_isolation_and_reserved_blocking() {
        let mut ext = ProviderExtensions::new();
        ext.insert(
            "anthropic",
            serde_json::json!({
                "headers": {
                    "X-Custom-Header": "safe-value",
                    "Authorization": "Bearer leaked_token",
                    "Host": "evil.com",
                    "anthropic-version": "2023-06-01"
                }
            }),
        );

        let safe = ext.safe_headers_for("anthropic");
        assert_eq!(safe.get("X-Custom-Header").unwrap(), "safe-value");
        assert_eq!(safe.get("anthropic-version").unwrap(), "2023-06-01");
        assert!(!safe.contains_key("Authorization"));
        assert!(!safe.contains_key("Host"));

        // Namespaced isolation: openai sees nothing
        let openai_safe = ext.safe_headers_for("openai");
        assert!(openai_safe.is_empty());
    }
}
