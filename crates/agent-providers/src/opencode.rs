use agent_core::{
    CompletionRequest, CompletionResponse, ContentBlock, EventStream, LlmProvider, Message,
    ModelCapabilities, ProviderError, Role, StreamEvent, TokenUsage, ToolDefinition,
};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const DEFAULT_OPENCODE_URL: &str = "http://127.0.0.1:4096";
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash-free";

pub const FALLBACK_FREE_MODELS: &[&str] = &[
    "deepseek-v4.1-flash-free",
    "glm-5.3-flash-free",
    "nemotron-3.5-lightning-free",
    "mimo-v2.6-flash-free",
];

/// Native OpenCode provider communicating directly with local `opencode serve --port 4096`
/// without requiring Node.js or intermediary proxy bridges.
#[derive(Clone)]
pub struct OpenCodeProvider {
    client: reqwest::Client,
    base_url: String,
    auto_start: bool,
    active_session_id: Arc<Mutex<Option<String>>>,
}

impl OpenCodeProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        let url = base_url.into();
        let trimmed_url = if url.is_empty() {
            DEFAULT_OPENCODE_URL.to_string()
        } else {
            url.trim_end_matches('/').to_string()
        };

        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            base_url: trimmed_url,
            auto_start: true,
            active_session_id: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }

    /// Extract port number from base URL
    pub fn parse_port(url_str: &str) -> u16 {
        if let Some(pos) = url_str.rfind(':') {
            let port_part = &url_str[pos + 1..];
            let digits: String = port_part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u16>().unwrap_or(4096)
        } else {
            4096
        }
    }

    /// Resolve the path to `opencode` executable across standard locations
    pub fn resolve_opencode_executable() -> Option<PathBuf> {
        if let Ok(path_str) = std::env::var("OPENCODE_EXE") {
            let p = PathBuf::from(path_str);
            if p.is_file() {
                return Some(p);
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                let winget_path = Path::new(&local_app_data)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Packages")
                    .join("SST.opencode_Microsoft.Winget.Source_8wekyb3d8bbwe")
                    .join("opencode.exe");
                if winget_path.is_file() {
                    return Some(winget_path);
                }
            }
        }

        // Check system PATH
        if let Some(path) = which_path("opencode") {
            return Some(path);
        }

        None
    }

    /// Probe if the OpenCode daemon is alive and responding
    pub async fn is_server_listening(&self) -> bool {
        let health_url = format!("{}/global/health", self.base_url);
        if let Ok(res) = self
            .client
            .get(&health_url)
            .timeout(Duration::from_millis(1500))
            .send()
            .await
        {
            if res.status().is_success() {
                return true;
            }
        }

        let doc_url = format!("{}/doc", self.base_url);
        if let Ok(res) = self
            .client
            .get(&doc_url)
            .timeout(Duration::from_millis(1500))
            .send()
            .await
        {
            return res.status().is_success();
        }

        false
    }

    /// Ensure the OpenCode background daemon is running, launching it if needed
    pub async fn ensure_server(&self) -> Result<(), ProviderError> {
        if self.is_server_listening().await {
            return Ok(());
        }

        if !self.auto_start {
            return Err(ProviderError::NetworkError(format!(
                "OpenCode daemon not detected at {}. Auto-start is disabled.",
                self.base_url
            )));
        }

        let exe = Self::resolve_opencode_executable().ok_or_else(|| {
            ProviderError::NetworkError(
                "OpenCode executable not found. Please install opencode or start it manually with 'opencode serve --port 4096'".into()
            )
        })?;

        let port = Self::parse_port(&self.base_url);
        tracing::info!("Launching background OpenCode engine on port {port}...");

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new(exe)
                .args(["serve", "--port", &port.to_string()])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .spawn()
                .map_err(|e| ProviderError::NetworkError(format!("Failed to spawn opencode: {e}")))?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new(exe)
                .args(["serve", "--port", &port.to_string()])
                .spawn()
                .map_err(|e| ProviderError::NetworkError(format!("Failed to spawn opencode: {e}")))?;
        }

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.is_server_listening().await {
                tracing::info!("Successfully connected to OpenCode daemon at {}", self.base_url);
                return Ok(());
            }
        }

        Err(ProviderError::NetworkError(format!(
            "Timed out waiting for OpenCode server to start on port {port}"
        )))
    }

    /// Retrieve or create a persistent session on the OpenCode server
    pub async fn get_or_create_session(&self) -> Result<String, ProviderError> {
        let mut guard = self.active_session_id.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }

        self.ensure_server().await?;

        let session_url = format!("{}/session", self.base_url);
        let res = self
            .client
            .post(&session_url)
            .json(&json!({ "directory": "." }))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(format!("Failed to create OpenCode session: {e}")))?;

        if !res.status().is_success() {
            let err = res.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidRequest(format!(
                "OpenCode session creation failed: {err}"
            )));
        }

        let data: serde_json::Value = res
            .json()
            .await
            .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;

        let id = data["id"]
            .as_str()
            .ok_or_else(|| ProviderError::MalformedResponse("Missing 'id' in OpenCode session response".into()))?
            .to_string();

        *guard = Some(id.clone());
        Ok(id)
    }

    /// Format tool definitions into instructions for OpenCode
    pub fn format_tools_instruction(tools: &[ToolDefinition]) -> String {
        if tools.is_empty() {
            return String::new();
        }

        let tool_list: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema
                })
            })
            .collect();

        format!(
            "\n\n### ACTIVE FERIC AGENT TOOLS\nYou have active access to the following registered tools:\n<tools>\n{}\n</tools>\n\nWhenever a task requires action, invoke the tool by outputting:\n<tool_call>\n{{\"name\": \"tool_name\", \"arguments\": {{\"param\": \"value\"}}}}\n</tool_call>\n",
            serde_json::to_string_pretty(&tool_list).unwrap_or_default()
        )
    }

    /// Parse both native OpenCode tool parts and fallback <tool_call> XML blocks using pure string parsing
    pub fn extract_tool_calls_and_text(
        raw_text: &str,
    ) -> (String, Vec<(String, String, serde_json::Value)>) {
        let mut tool_calls = Vec::new();
        let mut clean_text = String::new();
        let mut remaining = raw_text;
        let mut idx = 0;

        let start_tag = "<tool_call>";
        let end_tag = "</tool_call>";

        while let Some(start_pos) = remaining.find(start_tag) {
            clean_text.push_str(&remaining[..start_pos]);
            let after_start = &remaining[start_pos + start_tag.len()..];

            if let Some(end_pos) = after_start.find(end_tag) {
                let json_str = after_start[..end_pos].trim();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = parsed["name"]
                        .as_str()
                        .or_else(|| parsed["function"].as_str())
                        .unwrap_or("")
                        .to_string();

                    let args = parsed.get("arguments").cloned().unwrap_or_else(|| {
                        parsed.get("args").cloned().unwrap_or(json!({}))
                    });

                    if !name.is_empty() {
                        let now_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let call_id = format!("call_{now_ms}_{idx}");
                        tool_calls.push((call_id, name, args));
                        idx += 1;
                    }
                }
                remaining = &after_start[end_pos + end_tag.len()..];
            } else {
                clean_text.push_str(&remaining[start_pos..]);
                remaining = "";
                break;
            }
        }
        clean_text.push_str(remaining);

        (clean_text.trim().to_string(), tool_calls)
    }
}

fn which_path(program: &str) -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for p in std::env::split_paths(&path_var) {
            let candidate = p.join(program);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(target_os = "windows")]
            {
                let candidate_exe = p.join(format!("{program}.exe"));
                if candidate_exe.is_file() {
                    return Some(candidate_exe);
                }
            }
        }
    }
    None
}

#[async_trait]
impl LlmProvider for OpenCodeProvider {
    fn id(&self) -> &str {
        "opencode"
    }

    fn name(&self) -> &str {
        "OpenCode (Local Free Gateway)"
    }

    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            structured_output: true,
            vision: false,
            max_context_tokens: 128_000,
            max_output_tokens: 8_192,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let session_id = self.get_or_create_session().await?;

        // 1. Prepare system prompt and conversation turns
        let mut system = req.system_prompt.unwrap_or_default();
        if !req.tools.is_empty() {
            system.push_str(&Self::format_tools_instruction(&req.tools));
        }

        let mut conversation_turns = Vec::new();
        for msg in &req.messages {
            match msg.role {
                Role::System => {
                    for block in &msg.content {
                        if let ContentBlock::Text { text } = block {
                            system.push_str("\n\n");
                            system.push_str(text);
                        }
                    }
                }
                Role::User => {
                    let mut text = String::new();
                    for block in &msg.content {
                        if let ContentBlock::Text { text: t } = block {
                            text.push_str(t);
                        }
                    }
                    conversation_turns.push(format!("User: {text}"));
                }
                Role::Assistant => {
                    let mut text = String::new();
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text: t } => text.push_str(t),
                            ContentBlock::ToolUse { name, input, .. } => {
                                text.push_str(&format!(
                                    "\n<tool_call>\n{}\n</tool_call>",
                                    json!({ "name": name, "arguments": input })
                                ));
                            }
                            _ => {}
                        }
                    }
                    conversation_turns.push(format!("Assistant: {text}"));
                }
            }
        }

        let prompt_text = conversation_turns.join("\n\n");
        let model_id = if req.model.is_empty() || req.model == "default" {
            DEFAULT_MODEL
        } else {
            &req.model
        };

        let message_url = format!("{}/session/{session_id}/message", self.base_url);

        let mut models_to_try = vec![model_id];
        for fallback in FALLBACK_FREE_MODELS {
            if *fallback != model_id {
                models_to_try.push(fallback);
            }
        }

        let mut last_error = None;
        let mut response_data: Option<serde_json::Value> = None;

        for current_model in models_to_try {
            let payload = json!({
                "model": {
                    "providerID": "opencode",
                    "modelID": current_model,
                },
                "system": if system.is_empty() { None } else { Some(&system) },
                "parts": [
                    {
                        "type": "text",
                        "text": prompt_text,
                    }
                ]
            });

            match self
                .client
                .post(&message_url)
                .json(&payload)
                .timeout(Duration::from_secs(120))
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => {
                    if let Ok(data) = res.json::<serde_json::Value>().await {
                        response_data = Some(data);
                        break;
                    }
                }
                Ok(res) => {
                    let status = res.status();
                    let err = res.text().await.unwrap_or_default();
                    last_error = Some(ProviderError::NetworkError(format!(
                        "OpenCode model {current_model} failed (HTTP {status}): {err}"
                    )));
                }
                Err(e) => {
                    last_error = Some(ProviderError::NetworkError(format!(
                        "OpenCode network error: {e}"
                    )));
                }
            }
        }

        let data = response_data.ok_or_else(|| {
            last_error.unwrap_or_else(|| ProviderError::NetworkError("All OpenCode models exhausted".into()))
        })?;

        let mut content = Vec::new();
        let mut raw_text = String::new();

        if let Some(parts) = data["parts"].as_array() {
            for part in parts {
                let p_type = part["type"].as_str().unwrap_or("");
                if p_type == "text" {
                    if let Some(t) = part["text"].as_str() {
                        raw_text.push_str(t);
                    }
                } else if p_type == "tool-call" || p_type == "tool_use" {
                    if let Some(name) = part["tool"].as_str() {
                        let id = part["callID"]
                            .as_str()
                            .unwrap_or("call_direct")
                            .to_string();
                        let input = part.get("args").cloned().unwrap_or(json!({}));
                        content.push(ContentBlock::ToolUse {
                            id,
                            name: name.to_string(),
                            input,
                        });
                    }
                }
            }
        }

        // Parse fallback XML tool calls from text
        let (cleaned_text, extracted_calls) = Self::extract_tool_calls_and_text(&raw_text);
        if !cleaned_text.is_empty() {
            content.insert(0, ContentBlock::Text { text: cleaned_text });
        }
        for (id, name, input) in extracted_calls {
            content.push(ContentBlock::ToolUse { id, name, input });
        }

        let usage = data.get("info").and_then(|info| info.get("tokens")).map(|t| TokenUsage {
            input_tokens: t["input"].as_u64().unwrap_or(0),
            output_tokens: t["output"].as_u64().unwrap_or(0),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        Ok(CompletionResponse {
            message_id: format!("opencode_{now_ms}"),
            message: Message {
                role: Role::Assistant,
                content,
            },
            usage,
        })
    }

    async fn stream(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        // Execute completion and yield normalized events
        let resp = self.complete(req).await?;

        let mut events = Vec::new();
        events.push(StreamEvent::MessageStarted {
            message_id: resp.message_id.clone(),
        });

        let mut tool_idx = 0;
        for block in resp.message.content {
            match block {
                ContentBlock::Text { text } => {
                    events.push(StreamEvent::TextDelta { text });
                }
                ContentBlock::ToolUse { id, name, input } => {
                    events.push(StreamEvent::ToolCallStarted {
                        index: tool_idx,
                        id: Some(id),
                        name: Some(name),
                    });
                    events.push(StreamEvent::ToolCallDelta {
                        index: tool_idx,
                        arguments_chunk: input.to_string(),
                    });
                    events.push(StreamEvent::ToolCallCompleted { index: tool_idx });
                    tool_idx += 1;
                }
                _ => {}
            }
        }

        events.push(StreamEvent::MessageCompleted { usage: resp.usage });

        let stream = futures::stream::iter(events.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port() {
        assert_eq!(OpenCodeProvider::parse_port("http://127.0.0.1:4096"), 4096);
        assert_eq!(OpenCodeProvider::parse_port("http://localhost:8080/v1"), 8080);
        assert_eq!(OpenCodeProvider::parse_port("http://localhost"), 4096);
    }

    #[test]
    fn test_format_tools_instruction() {
        let tools = vec![ToolDefinition {
            name: "test_tool".into(),
            description: "A test tool".into(),
            input_schema: json!({ "type": "object" }),
        }];

        let instruction = OpenCodeProvider::format_tools_instruction(&tools);
        assert!(instruction.contains("ACTIVE FERIC AGENT TOOLS"));
        assert!(instruction.contains("test_tool"));
        assert!(instruction.contains("<tool_call>"));
    }

    #[test]
    fn test_extract_tool_calls_and_text() {
        let raw = "Here is the code:\n<tool_call>\n{\"name\": \"file_read\", \"arguments\": {\"path\": \"test.rs\"}}\n</tool_call>\nDone!";
        let (clean, calls) = OpenCodeProvider::extract_tool_calls_and_text(raw);
        assert_eq!(clean, "Here is the code:\nDone!");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "file_read");
        assert_eq!(calls[0].2["path"], "test.rs");
    }
}
