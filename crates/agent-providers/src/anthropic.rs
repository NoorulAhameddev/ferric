use agent_core::{
    CachePolicy, CompletionRequest, CompletionResponse, ContentBlock, EventStream, LlmProvider,
    Message, ModelCapabilities, ProviderError, Role, StreamEvent, TokenUsage,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::json;

pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
pub const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

pub type AnthropicPayloadWithHeaders = (serde_json::Value, Vec<(&'static str, String)>);

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: ANTHROPIC_DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Converts normalized CompletionRequest into Anthropic wire payload and headers.
    pub fn build_payload(
        &self,
        req: &CompletionRequest,
        stream: bool,
    ) -> Result<AnthropicPayloadWithHeaders, ProviderError> {
        let mut headers = vec![
            ("x-api-key", self.api_key.clone()),
            ("anthropic-version", ANTHROPIC_API_VERSION.to_string()),
            ("content-type", "application/json".to_string()),
        ];

        let mut betas = Vec::new();

        // 1. Prompt caching beta check
        if matches!(
            req.cache_policy,
            Some(CachePolicy::Prefer | CachePolicy::Require)
        ) {
            betas.push("prompt-caching-2024-07-25");
        }

        // 2. Thinking beta / budget check
        let mut thinking_json = None;
        if let Some(reasoning) = &req.reasoning {
            if let Some(budget) = reasoning.token_budget {
                betas.push("interleaved-thinking-2024-11-20");
                thinking_json = Some(json!({
                    "type": "enabled",
                    "budget_tokens": budget
                }));
            }
        }

        if !betas.is_empty() {
            headers.push(("anthropic-beta", betas.join(",")));
        }

        // 3. Messages transformation
        let mut wire_messages = Vec::new();
        for msg in &req.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => continue, // System is top-level in Anthropic
            };

            let mut content_parts = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        content_parts.push(json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        content_parts.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        content_parts.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content,
                            "is_error": is_error
                        }));
                    }
                }
            }

            wire_messages.push(json!({
                "role": role,
                "content": content_parts
            }));
        }

        // 4. Tools transformation
        let mut wire_tools = Vec::new();
        for tool in &req.tools {
            wire_tools.push(json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema
            }));
        }

        let max_tokens = req.generation.max_output_tokens.unwrap_or(4096);

        let mut body = json!({
            "model": req.model,
            "messages": wire_messages,
            "max_tokens": max_tokens,
            "stream": stream,
        });

        if let Some(sys) = &req.system_prompt {
            body["system"] = json!(sys);
        }

        if !wire_tools.is_empty() {
            body["tools"] = json!(wire_tools);
        }

        if let Some(temp) = req.generation.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(top_p) = req.generation.top_p {
            body["top_p"] = json!(top_p);
        }

        if let Some(thinking) = thinking_json {
            body["thinking"] = thinking;
        }

        // 5. Extensions override
        if let Some(anthropic_ext) = req.extensions.get("anthropic") {
            if let Some(obj) = anthropic_ext.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }

        Ok((body, headers))
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Anthropic"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        let is_high_output = model.contains("3-7") || model.contains("3.7");
        let is_medium_output = model.contains("3-5") || model.contains("3.5");

        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            structured_output: true,
            vision: true,
            max_context_tokens: 200_000,
            max_output_tokens: if is_high_output {
                64_000
            } else if is_medium_output {
                8_192
            } else {
                4_096
            },
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let (payload, headers) = self.build_payload(&req, false)?;
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let mut req_builder = self.client.post(&url);
        for (k, v) in headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let err_text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(ProviderError::AuthenticationFailed(err_text));
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let msg = if let Some(ra) = retry_after {
                    format!("rate_limit (retry-after: {ra}): {err_text}")
                } else {
                    err_text
                };
                return Err(ProviderError::RateLimitExceeded(msg));
            } else if status.as_u16() == 529 {
                return Err(ProviderError::ModelOverloaded(err_text));
            } else if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
                || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || status == reqwest::StatusCode::BAD_GATEWAY
            {
                return Err(ProviderError::NetworkError(format!(
                    "HTTP {status}: {err_text}"
                )));
            } else {
                return Err(ProviderError::InvalidRequest(format!(
                    "HTTP {status}: {err_text}"
                )));
            }
        }

        let wire_val: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;

        let message_id = wire_val["id"].as_str().unwrap_or("unknown_id").to_string();

        let mut content = Vec::new();
        if let Some(content_arr) = wire_val["content"].as_array() {
            for block in content_arr {
                match block["type"].as_str() {
                    Some("text") => {
                        content.push(ContentBlock::Text {
                            text: block["text"].as_str().unwrap_or("").to_string(),
                        });
                    }
                    Some("tool_use") => {
                        content.push(ContentBlock::ToolUse {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            input: block["input"].clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        let usage = wire_val.get("usage").map(|u| TokenUsage {
            input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
            cache_creation_input_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64()),
            cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
        });

        Ok(CompletionResponse {
            message_id,
            message: Message {
                role: Role::Assistant,
                content,
            },
            usage,
        })
    }

    async fn stream(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let (payload, headers) = self.build_payload(&req, true)?;
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        let mut req_builder = self.client.post(&url);
        for (k, v) in headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let err_text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(ProviderError::AuthenticationFailed(err_text));
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let msg = if let Some(ra) = retry_after {
                    format!("rate_limit (retry-after: {ra}): {err_text}")
                } else {
                    err_text
                };
                return Err(ProviderError::RateLimitExceeded(msg));
            } else if status.as_u16() == 529 {
                return Err(ProviderError::ModelOverloaded(err_text));
            } else if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
                || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || status == reqwest::StatusCode::BAD_GATEWAY
            {
                return Err(ProviderError::NetworkError(format!(
                    "HTTP {status}: {err_text}"
                )));
            } else {
                return Err(ProviderError::InvalidRequest(format!(
                    "HTTP {status}: {err_text}"
                )));
            }
        }

        let event_source = resp.bytes_stream().eventsource();

        let stream = event_source.map(|item| -> Result<StreamEvent, ProviderError> {
            let sse_event = item.map_err(|e| ProviderError::StreamError(e.to_string()))?;
            parse_anthropic_event(&sse_event.event, &sse_event.data)
        });

        Ok(Box::pin(stream))
    }
}

pub fn parse_anthropic_event(event_type: &str, data: &str) -> Result<StreamEvent, ProviderError> {
    let val: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::MalformedResponse(format!("{e}: {data}")))?;

    match event_type {
        "message_start" => {
            let msg_id = val["message"]["id"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            Ok(StreamEvent::MessageStarted { message_id: msg_id })
        }
        "content_block_start" => {
            let index = val["index"].as_u64().unwrap_or(0) as u32;
            let block = &val["content_block"];
            if block["type"].as_str() == Some("tool_use") {
                let id = block["id"].as_str().map(String::from);
                let name = block["name"].as_str().map(String::from);
                Ok(StreamEvent::ToolCallStarted { index, id, name })
            } else {
                Ok(StreamEvent::TextDelta {
                    text: String::new(),
                })
            }
        }
        "content_block_delta" => {
            let index = val["index"].as_u64().unwrap_or(0) as u32;
            let delta = &val["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    let text = delta["text"].as_str().unwrap_or("").to_string();
                    Ok(StreamEvent::TextDelta { text })
                }
                Some("thinking_delta") => {
                    let text = delta["thinking"].as_str().unwrap_or("").to_string();
                    Ok(StreamEvent::ThinkingDelta { text })
                }
                Some("input_json_delta") => {
                    let chunk = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Ok(StreamEvent::ToolCallDelta {
                        index,
                        arguments_chunk: chunk,
                    })
                }
                _ => Ok(StreamEvent::TextDelta {
                    text: String::new(),
                }),
            }
        }
        "content_block_stop" => {
            let index = val["index"].as_u64().unwrap_or(0) as u32;
            Ok(StreamEvent::ToolCallCompleted { index })
        }
        "message_delta" => {
            let usage = val.get("usage").map(|u| TokenUsage {
                input_tokens: 0,
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            Ok(StreamEvent::MessageCompleted { usage })
        }
        "message_stop" => Ok(StreamEvent::MessageCompleted { usage: None }),
        "error" => {
            let err = val["error"]["message"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string();
            Ok(StreamEvent::StreamError { error: err })
        }
        _ => Ok(StreamEvent::TextDelta {
            text: String::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ToolCallAccumulator;

    #[test]
    fn test_anthropic_event_parsing() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant"}}"#;
        assert_eq!(
            parse_anthropic_event("message_start", msg_start).unwrap(),
            StreamEvent::MessageStarted {
                message_id: "msg_123".into()
            }
        );

        let text_delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world!"}}"#;
        assert_eq!(
            parse_anthropic_event("content_block_delta", text_delta).unwrap(),
            StreamEvent::TextDelta {
                text: "Hello world!".into()
            }
        );
    }

    #[test]
    fn test_anthropic_tool_call_streaming_into_accumulator() {
        let block_start = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_999","name":"FileRead","input":{}}}"#;
        let delta_1 = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"Cargo"}}"#;
        let delta_2 = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":".toml\"}"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":1}"#;

        let ev_start = parse_anthropic_event("content_block_start", block_start).unwrap();
        let ev_delta1 = parse_anthropic_event("content_block_delta", delta_1).unwrap();
        let ev_delta2 = parse_anthropic_event("content_block_delta", delta_2).unwrap();
        let ev_stop = parse_anthropic_event("content_block_stop", block_stop).unwrap();

        let mut acc = ToolCallAccumulator::default();
        acc.process_event(&ev_start).unwrap();
        acc.process_event(&ev_delta1).unwrap();
        acc.process_event(&ev_delta2).unwrap();
        acc.process_event(&ev_stop).unwrap();

        let (msg, _) = acc.finalize().unwrap();
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_999");
                assert_eq!(name, "FileRead");
                assert_eq!(input["file_path"], "Cargo.toml");
            }
            _ => panic!("Expected ToolUse block"),
        }
    }
}
