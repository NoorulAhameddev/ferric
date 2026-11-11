use agent_core::{
    CompletionRequest, CompletionResponse, ContentBlock, EventStream, LlmProvider, Message,
    ModelCapabilities, ProviderError, Role, StreamEvent, TokenUsage,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::json;

pub const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: GEMINI_DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn build_payload(
        &self,
        req: &CompletionRequest,
    ) -> Result<serde_json::Value, ProviderError> {
        let mut contents = Vec::new();

        for msg in &req.messages {
            let role = match msg.role {
                Role::User | Role::System => "user",
                Role::Assistant => "model",
            };

            let mut parts = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        parts.push(json!({ "text": text }));
                    }
                    ContentBlock::ToolUse { id: _, name, input } => {
                        parts.push(json!({
                            "functionCall": {
                                "name": name,
                                "args": input
                            }
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id: _,
                        content,
                        is_error: _,
                    } => {
                        parts.push(json!({
                            "functionResponse": {
                                "name": "tool_result",
                                "response": { "content": content }
                            }
                        }));
                    }
                }
            }

            contents.push(json!({
                "role": role,
                "parts": parts
            }));
        }

        let mut body = json!({ "contents": contents });

        if let Some(sys) = &req.system_prompt {
            body["systemInstruction"] = json!({
                "parts": [{ "text": sys }]
            });
        }

        if !req.tools.is_empty() {
            let mut function_declarations = Vec::new();
            for tool in &req.tools {
                function_declarations.push(json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }));
            }
            body["tools"] = json!([{
                "functionDeclarations": function_declarations
            }]);
        }

        Ok(body)
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn id(&self) -> &str {
        "gemini"
    }

    fn name(&self) -> &str {
        "Google Gemini"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        let is_gemini_2 = model.contains("gemini-2") || model.contains("2.0");
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            structured_output: true,
            vision: true,
            max_context_tokens: if is_gemini_2 { 2_000_000 } else { 1_000_000 },
            max_output_tokens: 8_192,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let payload = self.build_payload(&req)?;
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            req.model,
            self.api_key
        );

        let resp = self
            .client
            .post(&url)
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

        let mut content = Vec::new();
        if let Some(candidate) = wire_val["candidates"].as_array().and_then(|c| c.first()) {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for part in parts {
                    if let Some(text) = part["text"].as_str() {
                        content.push(ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc["name"].as_str().unwrap_or("").to_string();
                        let args = fc.get("args").cloned().unwrap_or(json!({}));
                        content.push(ContentBlock::ToolUse {
                            id: format!("gemini_call_{name}"),
                            name,
                            input: args,
                        });
                    }
                }
            }
        }

        let usage = wire_val.get("usageMetadata").map(|u| TokenUsage {
            input_tokens: u["promptTokenCount"].as_u64().unwrap_or(0),
            output_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });

        Ok(CompletionResponse {
            message_id: "gemini_resp".to_string(),
            message: Message {
                role: Role::Assistant,
                content,
            },
            usage,
        })
    }

    async fn stream(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let payload = self.build_payload(&req)?;
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url.trim_end_matches('/'),
            req.model,
            self.api_key
        );

        let resp = self
            .client
            .post(&url)
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
                Ok(sse_event) => parse_gemini_chunk(sse_event.data.trim()),
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

pub fn parse_gemini_chunk(data: &str) -> Result<Vec<StreamEvent>, ProviderError> {
    let val: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::MalformedResponse(format!("{e}: {data}")))?;

    let mut events = Vec::new();

    if let Some(candidate) = val["candidates"].as_array().and_then(|c| c.first()) {
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    if !text.is_empty() {
                        events.push(StreamEvent::TextDelta {
                            text: text.to_string(),
                        });
                    }
                }
                if let Some(fc) = part.get("functionCall") {
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    let args = fc.get("args").cloned().unwrap_or(json!({}));
                    let args_str = serde_json::to_string(&args).unwrap_or_default();

                    events.push(StreamEvent::ToolCallStarted {
                        index: 0,
                        id: Some(format!("gemini_call_{name}")),
                        name: Some(name),
                    });
                    events.push(StreamEvent::ToolCallDelta {
                        index: 0,
                        arguments_chunk: args_str,
                    });
                    events.push(StreamEvent::ToolCallCompleted { index: 0 });
                }
            }
        }

        if let Some(finish_reason) = candidate["finishReason"].as_str() {
            if finish_reason == "STOP" {
                let usage = val.get("usageMetadata").map(|u| TokenUsage {
                    input_tokens: u["promptTokenCount"].as_u64().unwrap_or(0),
                    output_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                });
                events.push(StreamEvent::MessageCompleted { usage });
            }
        }
    }

    Ok(events)
}
