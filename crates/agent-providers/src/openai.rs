use agent_core::{
    CompletionRequest, CompletionResponse, ContentBlock, EventStream, LlmProvider, Message,
    ModelCapabilities, ProviderError, ReasoningEffort, Role, StreamEvent, TokenUsage,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::json;

pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: OPENAI_DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn build_payload(
        &self,
        req: &CompletionRequest,
        stream: bool,
    ) -> Result<serde_json::Value, ProviderError> {
        let mut wire_messages = Vec::new();

        if let Some(sys) = &req.system_prompt {
            wire_messages.push(json!({
                "role": "system",
                "content": sys
            }));
        }

        for msg in &req.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };

            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();

            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(input).unwrap_or_default()
                            }
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        wire_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content
                        }));
                    }
                }
            }

            if !text_parts.is_empty() || !tool_calls.is_empty() {
                let mut wire_msg = json!({
                    "role": role,
                    "content": text_parts.join("\n")
                });
                if !tool_calls.is_empty() {
                    wire_msg["tool_calls"] = json!(tool_calls);
                }
                wire_messages.push(wire_msg);
            }
        }

        let mut wire_tools = Vec::new();
        for tool in &req.tools {
            wire_tools.push(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            }));
        }

        let mut body = json!({
            "model": req.model,
            "messages": wire_messages,
            "stream": stream,
        });

        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }

        if !wire_tools.is_empty() {
            body["tools"] = json!(wire_tools);
        }

        if let Some(max) = req.generation.max_output_tokens {
            body["max_tokens"] = json!(max);
        }

        if let Some(temp) = req.generation.temperature {
            body["temperature"] = json!(temp);
        }

        // Reasoning effort mapping for o1/o3/o-series models
        if let Some(reasoning) = &req.reasoning {
            if let Some(effort) = reasoning.effort {
                let effort_str = match effort {
                    ReasoningEffort::Low => "low",
                    ReasoningEffort::Medium => "medium",
                    ReasoningEffort::High => "high",
                };
                body["reasoning_effort"] = json!(effort_str);
            }
        }

        // Extensions override
        if let Some(openai_ext) = req.extensions.get("openai") {
            if let Some(obj) = openai_ext.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }

        Ok(body)
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn name(&self) -> &str {
        "OpenAI"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        let is_reasoning = model.starts_with("o1") || model.starts_with("o3");
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            structured_output: true,
            vision: true,
            max_context_tokens: if is_reasoning { 200_000 } else { 128_000 },
            max_output_tokens: if is_reasoning { 64_000 } else { 16_384 },
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let payload = self.build_payload(&req, false)?;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
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
        let choice = &wire_val["choices"][0]["message"];

        let mut content = Vec::new();
        if let Some(text) = choice["content"].as_str() {
            if !text.is_empty() {
                content.push(ContentBlock::Text {
                    text: text.to_string(),
                });
            }
        }

        if let Some(tool_calls) = choice["tool_calls"].as_array() {
            for tc in tool_calls {
                let id = tc["id"].as_str().unwrap_or("").to_string();
                let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let input: serde_json::Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }

        let usage = wire_val.get("usage").map(|u| TokenUsage {
            input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
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
        let payload = self.build_payload(&req, true)?;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
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

        let stream = event_source.flat_map(|item| {
            let res = match item {
                Ok(sse_event) => parse_openai_chunk(sse_event.data.trim()),
                Err(e) => Err(ProviderError::StreamError(e.to_string())),
            };
            match res {
                Ok(events) => futures::stream::iter(events.into_iter().map(Ok)).left_stream(),
                Err(e) => futures::stream::once(async move { Err(e) }).right_stream(),
            }
        });

        Ok(Box::pin(stream))
    }
}

pub fn parse_openai_chunk(data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
    if data == "[DONE]" {
        return Ok(vec![StreamEvent::MessageCompleted { usage: None }]);
    }

    let val: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::MalformedResponse(format!("{e}: {data}")))?;

    let mut events = Vec::new();

    if let Some(choices) = val["choices"].as_array() {
        if let Some(choice) = choices.first() {
            let delta = &choice["delta"];

            // 0. Reasoning / Thinking delta (DeepSeek, OpenRouter, Nemotron, etc.)
            let reasoning_chunk = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
                .or_else(|| delta["thought"].as_str());
            if let Some(r_chunk) = reasoning_chunk {
                if !r_chunk.is_empty() {
                    events.push(StreamEvent::ThinkingDelta {
                        text: r_chunk.to_string(),
                    });
                }
            }

            // 1. Text delta
            if let Some(content_chunk) = delta["content"].as_str() {
                if !content_chunk.is_empty() {
                    events.push(StreamEvent::TextDelta {
                        text: content_chunk.to_string(),
                    });
                }
            }

            // 2. Tool call delta
            if let Some(tool_calls) = delta["tool_calls"].as_array() {
                if let Some(tc) = tool_calls.first() {
                    let index = tc["index"].as_u64().unwrap_or(0) as u32;

                    if let Some(name) = tc["function"]["name"].as_str() {
                        let id = tc["id"].as_str().map(String::from);
                        events.push(StreamEvent::ToolCallStarted {
                            index,
                            id,
                            name: Some(name.to_string()),
                        });
                    }

                    if let Some(args_chunk) = tc["function"]["arguments"].as_str() {
                        events.push(StreamEvent::ToolCallDelta {
                            index,
                            arguments_chunk: args_chunk.to_string(),
                        });
                    }
                }
            }

            // 3. Completion
            if let Some(finish_reason) = choice["finish_reason"].as_str() {
                if finish_reason == "tool_calls" || finish_reason == "stop" {
                    events.push(StreamEvent::ToolCallCompleted { index: 0 });
                }
            }
        }
    }

    // 4. Usage in stream
    if let Some(usage_val) = val.get("usage") {
        if !usage_val.is_null() {
            let usage = TokenUsage {
                input_tokens: usage_val["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: usage_val["completion_tokens"].as_u64().unwrap_or(0),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            };
            events.push(StreamEvent::MessageCompleted { usage: Some(usage) });
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ToolCallAccumulator;

    #[test]
    fn test_openai_text_stream() {
        let chunk = r#"{"id":"chatcmpl-1","choices":[{"delta":{"content":"Hello from OpenAI!"},"index":0}]}"#;
        let events = parse_openai_chunk(chunk).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::TextDelta {
                text: "Hello from OpenAI!".into()
            }
        );
    }

    #[test]
    fn test_openai_tool_call_streaming_into_accumulator() {
        let chunk_start = r#"{"id":"chatcmpl-2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"FileWrite","arguments":""}}]},"index":0}]}"#;
        let chunk_arg1 = r#"{"id":"chatcmpl-2","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"test.txt\","}}]},"index":0}]}"#;
        let chunk_arg2 = r#"{"id":"chatcmpl-2","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"content\":\"ok\"}"}}]},"index":0}]}"#;
        let chunk_finish = r#"{"id":"chatcmpl-2","choices":[{"delta":{},"finish_reason":"tool_calls","index":0}]}"#;

        let mut acc = ToolCallAccumulator::default();
        for ev in parse_openai_chunk(chunk_start).unwrap() {
            acc.process_event(&ev).unwrap();
        }
        for ev in parse_openai_chunk(chunk_arg1).unwrap() {
            acc.process_event(&ev).unwrap();
        }
        for ev in parse_openai_chunk(chunk_arg2).unwrap() {
            acc.process_event(&ev).unwrap();
        }
        for ev in parse_openai_chunk(chunk_finish).unwrap() {
            acc.process_event(&ev).unwrap();
        }

        let (msg, _) = acc.finalize().unwrap();
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "FileWrite");
                assert_eq!(input["path"], "test.txt");
                assert_eq!(input["content"], "ok");
            }
            _ => panic!("Expected ToolUse block"),
        }
    }
}
