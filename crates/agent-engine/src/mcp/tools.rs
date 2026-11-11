use super::client::McpClient;
use super::validation::EndpointValidator;
use crate::tools::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Adapts an MCP tool into the native agent-engine Tool trait
pub struct McpToolWrapper {
    server_name: String,
    tool_name: String,
    description: String,
    input_schema: Value,
    client: Arc<Mutex<McpClient>>,
}

impl McpToolWrapper {
    pub fn new(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        client: Arc<Mutex<McpClient>>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            description: description.into(),
            input_schema,
            client,
        }
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn definition(&self) -> ToolDefinition {
        // Namespaced tool name to prevent collisions across multiple MCP servers
        let qualified_name = format!("{}__{}", self.server_name, self.tool_name);
        ToolDefinition {
            name: qualified_name,
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }

    async fn execute(&self, input: Value, _workspace_root: &Path) -> Result<String, String> {
        // 1. Strict schema validation of input arguments before invocation
        EndpointValidator::validate_tool_arguments(&self.input_schema, &input)
            .map_err(|e| format!("MCP Tool validation error: {e}"))?;

        // 2. Invoke tool via MCP client
        let client = self.client.lock().await;
        client
            .call_tool(&self.tool_name, input)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
    use crate::mcp::transport::{McpTransport, McpTransportError};
    use serde_json::json;

    struct MockMcpTransport;

    #[async_trait]
    impl McpTransport for MockMcpTransport {
        async fn send_request(
            &self,
            req: JsonRpcRequest,
        ) -> Result<JsonRpcResponse, McpTransportError> {
            match req.method.as_str() {
                "initialize" => Ok(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: req.id,
                    result: Some(json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": { "name": "mock-server", "version": "1.0.0" },
                        "capabilities": { "tools": {} }
                    })),
                    error: None,
                }),
                "tools/call" => {
                    let args = req.params.unwrap();
                    let name = args["name"].as_str().unwrap();
                    let input = &args["arguments"];

                    let query = input["query"].as_str().unwrap_or("");
                    Ok(JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: req.id,
                        result: Some(json!({
                            "content": [
                                { "type": "text", "text": format!("Executed {name} with arg {query}") }
                            ],
                            "isError": false
                        })),
                        error: None,
                    })
                }
                _ => Ok(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: req.id,
                    result: Some(json!({})),
                    error: None,
                }),
            }
        }

        async fn close(&self) -> Result<(), McpTransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_mcp_tool_wrapper_execution() {
        let transport = Arc::new(MockMcpTransport);
        let mut client = McpClient::new(transport);
        client.initialize().await.unwrap();

        let client_arc = Arc::new(Mutex::new(client));
        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"]
        });

        let wrapper = McpToolWrapper::new(
            "github",
            "search_repos",
            "Search GitHub repositories",
            schema,
            client_arc,
        );

        let def = wrapper.definition();
        assert_eq!(def.name, "github__search_repos");

        // Execution with valid arguments
        let output = wrapper
            .execute(json!({ "query": "antigravity" }), Path::new("."))
            .await
            .unwrap();
        assert!(output.contains("Executed search_repos with arg antigravity"));

        // Execution with missing arguments fails validation
        let err = wrapper.execute(json!({}), Path::new(".")).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Missing required argument"));
    }
}
