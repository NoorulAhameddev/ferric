use super::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use super::validation::{EndpointValidator, McpValidationError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum McpTransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Validation error: {0}")]
    Validation(#[from] McpValidationError),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Process exited with code {0}")]
    ProcessExited(i32),
    #[error("Transport closed")]
    Closed,
}

#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn send_request(&self, req: JsonRpcRequest)
        -> Result<JsonRpcResponse, McpTransportError>;
    async fn close(&self) -> Result<(), McpTransportError>;
}

// ---------------------------------------------------------------------------
// 1. Stdio Transport (Local Subprocess with Job Object Protection)
// ---------------------------------------------------------------------------
pub struct StdioTransport {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout_reader: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    child: Arc<Mutex<tokio::process::Child>>,
    #[cfg(target_os = "windows")]
    _job: Option<super::super::tools::win_job::JobObjectGuard>,
}

impl StdioTransport {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: &Path,
    ) -> Result<Self, McpTransportError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        cmd.envs(env);
        cmd.current_dir(working_dir);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        #[cfg(target_os = "windows")]
        use std::os::windows::process::CommandExt;
        #[cfg(target_os = "windows")]
        cmd.as_std_mut().creation_flags(0x00000004); // CREATE_SUSPENDED

        #[cfg(target_os = "windows")]
        let job = super::super::tools::win_job::JobObjectGuard::create().ok();

        let mut child = cmd.spawn()?;

        #[cfg(target_os = "windows")]
        if let Some(ref j) = job {
            if let Some(raw_handle) = child.raw_handle() {
                let h = raw_handle as windows_sys::Win32::Foundation::HANDLE;
                if let Err(e) = j.assign_handle(h) {
                    tracing::error!("Failed to assign MCP process to Job Object: {e}");
                    unsafe {
                        windows_sys::Win32::System::Threading::TerminateProcess(h, 1);
                    }
                    return Err(McpTransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Process security initialization failed: {e}"),
                    )));
                }
                let _ = j.resume_handle(h);
            }
        }

        let stdin = child.stdin.take().ok_or_else(|| {
            McpTransportError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Failed to capture child stdin",
            ))
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            McpTransportError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Failed to capture child stdout",
            ))
        })?;

        // Drain stderr asynchronously to avoid deadlocks while isolating server logs from protocol frames
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::debug!(target: "mcp_stderr", "{}", line);
                }
            });
        }

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            stdout_reader: Arc::new(Mutex::new(BufReader::new(stdout))),
            child: Arc::new(Mutex::new(child)),
            #[cfg(target_os = "windows")]
            _job: job,
        })
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send_request(
        &self,
        req: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, McpTransportError> {
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        // Write newline-delimited frame to stdin
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        // Read response line from stdout
        let mut stdout = self.stdout_reader.lock().await;
        let mut response_line = String::new();
        let bytes_read = stdout.read_line(&mut response_line).await?;

        if bytes_read == 0 {
            return Err(McpTransportError::Closed);
        }

        let response: JsonRpcResponse = serde_json::from_str(&response_line)?;
        Ok(response)
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 2. Streamable HTTP Transport (Remote MCP Server with SSRF Protection)
// ---------------------------------------------------------------------------
pub struct StreamableHttpTransport {
    client: reqwest::Client,
    endpoint: url::Url,
    headers: HashMap<String, String>,
}

impl StreamableHttpTransport {
    pub fn new(
        raw_url: &str,
        headers: HashMap<String, String>,
        allow_localhost: bool,
    ) -> Result<Self, McpTransportError> {
        let validator = EndpointValidator::new(allow_localhost);
        let (validated_endpoint, pinned_addr) = validator.validate_and_pin_endpoint(raw_url)?;

        let mut client_builder = reqwest::Client::builder()
            // Strictly disallow cross-origin or transparent redirects to eliminate redirect-based SSRF bypass
            .redirect(reqwest::redirect::Policy::none());

        // IP Pinning: Bind the resolved hostname to the exact pre-validated SocketAddr
        // This eliminates Time-of-Check / Time-of-Use (TOCTOU) DNS rebinding attacks entirely.
        if let Some(socket_addr) = pinned_addr {
            if let Some(host_str) = validated_endpoint.host_str() {
                client_builder = client_builder.resolve(host_str, socket_addr);
            }
        }

        let client = client_builder
            .build()
            .map_err(|e| McpTransportError::Http(e.to_string()))?;

        Ok(Self {
            client,
            endpoint: validated_endpoint,
            headers,
        })
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn send_request(
        &self,
        req: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, McpTransportError> {
        let mut req_builder = self
            .client
            .post(self.endpoint.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&req);

        for (k, v) in &self.headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder
            .send()
            .await
            .map_err(|e| McpTransportError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(McpTransportError::Http(format!("HTTP {status}: {body}")));
        }

        let rpc_resp: JsonRpcResponse = resp
            .json()
            .await
            .map_err(|e| McpTransportError::Http(format!("Malformed JSON-RPC response: {e}")))?;

        Ok(rpc_resp)
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        Ok(())
    }
}
