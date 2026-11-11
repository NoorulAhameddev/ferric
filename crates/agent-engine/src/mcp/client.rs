use super::jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId};
use super::transport::{McpTransport, McpTransportError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const LATEST_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(#[from] McpTransportError),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Server error ({code}): {message}")]
    ServerError { code: i64, message: String },
    #[error("Tool execution failed: {0}")]
    ToolExecutionFailed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub resources: Option<Value>,
    #[serde(default)]
    pub prompts: Option<Value>,
    #[serde(default)]
    pub logging: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

pub struct McpClient {
    transport: Arc<dyn McpTransport>,
    next_id: AtomicU64,
    server_info: Option<ServerInfo>,
    server_capabilities: Option<ServerCapabilities>,
}

impl McpClient {
    pub fn new(transport: Arc<dyn McpTransport>) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
            server_info: None,
            server_capabilities: None,
        }
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Performs the initial MCP protocol handshake
    pub async fn initialize(&mut self) -> Result<(), McpError> {
        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "initialize",
            Some(json!({
                "protocolVersion": LATEST_MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "clientInfo": {
                    "name": "agent-rust",
                    "version": "0.1.0"
                }
            })),
        );

        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::ServerError {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp.result.ok_or_else(|| {
            McpError::Protocol("Initialize response missing 'result' object".to_string())
        })?;

        let server_info: ServerInfo = serde_json::from_value(result["serverInfo"].clone())
            .unwrap_or_else(|_| ServerInfo {
                name: "unknown".into(),
                version: "0.0.0".into(),
            });

        let server_capabilities: ServerCapabilities =
            serde_json::from_value(result["capabilities"].clone()).unwrap_or(ServerCapabilities {
                tools: None,
                resources: None,
                prompts: None,
                logging: None,
            });

        self.server_info = Some(server_info);
        self.server_capabilities = Some(server_capabilities);

        // Send notifications/initialized as required by the MCP protocol spec
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        let req_notif = JsonRpcRequest::new(self.next_request_id(), notif.method, notif.params);
        let _ = self.transport.send_request(req_notif).await;

        Ok(())
    }

    /// Discovers all tools exposed by the MCP server
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let req = JsonRpcRequest::new(self.next_request_id(), "tools/list", None);
        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::ServerError {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| McpError::Protocol("tools/list missing 'result'".into()))?;

        let tools: Vec<McpToolInfo> = serde_json::from_value(result["tools"].clone())
            .map_err(|e| McpError::Protocol(format!("Failed to parse tools list: {e}")))?;

        Ok(tools)
    }

    /// Invokes a discovered MCP tool with validated arguments
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        );

        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::ServerError {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| McpError::Protocol("tools/call missing 'result'".into()))?;

        let is_error = result["isError"].as_bool().unwrap_or(false);

        let mut output = String::new();
        if let Some(content_items) = result["content"].as_array() {
            for item in content_items {
                if let Some(text) = item["text"].as_str() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(text);
                }
            }
        }

        if is_error {
            Err(McpError::ToolExecutionFailed(output))
        } else {
            Ok(output)
        }
    }

    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }
}
