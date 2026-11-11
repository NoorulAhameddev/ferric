use crate::loop_engine::AgentLoop;
use crate::permissions::PermissionPolicy;
use crate::tasks::TaskManager;
use crate::tools::Tool;
use agent_core::{LlmProvider, ToolDefinition};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Subagent Identifiers and Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubagentId(pub String);

impl SubagentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubagentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for SubagentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SubagentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for SubagentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Completed { result: String },
    Failed { error: String },
    Cancelled,
}

impl SubagentStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, SubagentStatus::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInfo {
    pub id: SubagentId,
    pub description: String,
    pub status: SubagentStatus,
    pub created_at: DateTime<Utc>,
    pub messages_delivered: usize,
}

struct SubagentHandle {
    info: SubagentInfo,
    message_tx: mpsc::Sender<String>,
    cancel_token: CancellationToken,
}

// ---------------------------------------------------------------------------
// Subagent Manager
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct SubagentManager {
    agents: Arc<Mutex<HashMap<SubagentId, SubagentHandle>>>,
    counter: Arc<AtomicU64>,
}

impl SubagentManager {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Spawns a subagent asynchronously with message receiver and cancellation support.
    pub fn spawn_subagent(
        &self,
        description: impl Into<String>,
        mut child_loop: AgentLoop,
        prompt: String,
    ) -> SubagentId {
        let count = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = SubagentId::new(format!("subagent-{count}"));
        let (tx, mut rx) = mpsc::channel::<String>(32);
        let token = CancellationToken::new();

        let desc = description.into();
        let info = SubagentInfo {
            id: id.clone(),
            description: desc,
            status: SubagentStatus::Running,
            created_at: Utc::now(),
            messages_delivered: 0,
        };

        if let Ok(mut agents) = self.agents.lock() {
            agents.insert(
                id.clone(),
                SubagentHandle {
                    info,
                    message_tx: tx,
                    cancel_token: token.clone(),
                },
            );
        }

        let agents_clone = self.agents.clone();
        let agent_id_clone = id.clone();
        let token_clone = token.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = token_clone.cancelled() => {
                    if let Ok(mut agents) = agents_clone.lock() {
                        if let Some(handle) = agents.get_mut(&agent_id_clone) {
                            handle.info.status = SubagentStatus::Cancelled;
                        }
                    }
                }
                res = child_loop.run(&prompt) => {
                    match res {
                        Ok(mut result_text) => {
                            // Process any messages that queued up
                            while let Ok(msg) = rx.try_recv() {
                                if let Ok(reply) = child_loop.run(&msg).await {
                                    result_text.push('\n');
                                    result_text.push_str(&reply);
                                }
                            }

                            if let Ok(mut agents) = agents_clone.lock() {
                                if let Some(handle) = agents.get_mut(&agent_id_clone) {
                                    if handle.info.status == SubagentStatus::Running {
                                        handle.info.status = SubagentStatus::Completed { result: result_text };
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            if let Ok(mut agents) = agents_clone.lock() {
                                if let Some(handle) = agents.get_mut(&agent_id_clone) {
                                    if handle.info.status == SubagentStatus::Running {
                                        handle.info.status = SubagentStatus::Failed { error: err };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        id
    }

    /// Delivers an incoming message to a running subagent.
    pub fn send_message(&self, id: &SubagentId, message: &str) -> Result<(), String> {
        let mut agents = self.agents.lock().map_err(|e| format!("Lock error: {e}"))?;
        let handle = agents
            .get_mut(id)
            .ok_or_else(|| format!("Subagent '{id}' not found"))?;

        if !handle.info.status.is_running() {
            return Err(format!(
                "Subagent '{id}' is not running (status: {:?})",
                handle.info.status
            ));
        }

        handle
            .message_tx
            .try_send(message.to_string())
            .map_err(|e| format!("Failed to deliver message to subagent '{id}': {e}"))?;

        handle.info.messages_delivered += 1;
        Ok(())
    }

    /// Stops/cancels an active subagent.
    pub fn stop_subagent(&self, id: &SubagentId) -> Result<(), String> {
        let mut agents = self.agents.lock().map_err(|e| format!("Lock error: {e}"))?;
        let handle = agents
            .get_mut(id)
            .ok_or_else(|| format!("Subagent '{id}' not found"))?;

        handle.cancel_token.cancel();
        if handle.info.status == SubagentStatus::Running {
            handle.info.status = SubagentStatus::Cancelled;
        }
        Ok(())
    }

    pub fn get_subagent(&self, id: &SubagentId) -> Option<SubagentInfo> {
        let agents = self.agents.lock().ok()?;
        agents.get(id).map(|h| h.info.clone())
    }

    pub fn list_subagents(&self) -> Vec<SubagentInfo> {
        if let Ok(agents) = self.agents.lock() {
            let mut list: Vec<SubagentInfo> = agents.values().map(|h| h.info.clone()).collect();
            list.sort_by_key(|a| a.created_at);
            list
        } else {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Agent Tool
// ---------------------------------------------------------------------------

pub struct AgentTool {
    provider: Arc<dyn LlmProvider>,
    policy: PermissionPolicy,
    sessions_dir: PathBuf,
    model: String,
    task_manager: Option<TaskManager>,
    subagent_manager: Option<SubagentManager>,
}

impl AgentTool {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        policy: PermissionPolicy,
        sessions_dir: impl Into<PathBuf>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            policy,
            sessions_dir: sessions_dir.into(),
            model: model.into(),
            task_manager: None,
            subagent_manager: None,
        }
    }

    pub fn from_agent_loop(agent_loop: &AgentLoop) -> Self {
        Self::new(
            agent_loop.provider(),
            agent_loop.policy().clone(),
            agent_loop.sessions_dir().to_path_buf(),
            agent_loop.model().to_string(),
        )
    }

    pub fn with_task_manager(mut self, task_manager: TaskManager) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    pub fn with_subagent_manager(mut self, subagent_manager: SubagentManager) -> Self {
        self.subagent_manager = Some(subagent_manager);
        self
    }

    /// Spawns a child AgentLoop with isolated messages and unique session.
    pub fn create_child_loop(&self, workspace_root: &Path) -> AgentLoop {
        let mut child = AgentLoop::new(
            self.provider.clone(),
            workspace_root.to_path_buf(),
            self.policy.clone(),
            self.sessions_dir.clone(),
            self.model.clone(),
        );

        if let Some(ref tm) = self.task_manager {
            child.register_tool(Box::new(crate::tasks::TaskCreateTool::new(tm.clone())));
            child.register_tool(Box::new(crate::tasks::TaskListTool::new(tm.clone())));
            child.register_tool(Box::new(crate::tasks::TaskGetTool::new(tm.clone())));
            child.register_tool(Box::new(crate::tasks::TaskStopTool::new(tm.clone())));
        }

        if let Some(ref sm) = self.subagent_manager {
            let mut send_msg = SendMessageTool::new().with_subagent_manager(sm.clone());
            if let Some(ref tm) = self.task_manager {
                send_msg = send_msg.with_task_manager(tm.clone());
            }
            child.register_tool(Box::new(send_msg));
        }

        child
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Agent".into(),
            description: "Spin up a child agent with an isolated conversation context to execute a sub-task and return the final response".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The task prompt or instructions to give to the subagent"
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Optional custom system prompt for the subagent"
                    },
                    "max_turns": {
                        "type": "integer",
                        "description": "Optional maximum turns for the subagent run (defaults to 20)"
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Optional flag to run subagent in background (returns subagent ID immediately)"
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let prompt = input["prompt"]
            .as_str()
            .ok_or("Missing 'prompt' argument")?;
        let system_prompt = input["system_prompt"].as_str();
        let max_turns = input["max_turns"].as_u64().map(|n| n as u32);
        let background = input["background"].as_bool().unwrap_or(false);

        let mut child = self.create_child_loop(workspace_root);
        if let Some(sys) = system_prompt {
            child.set_system_prompt(sys);
        }
        if let Some(turns) = max_turns {
            child.set_max_turns(turns);
        }

        if background {
            if let Some(ref sm) = self.subagent_manager {
                let desc = format!(
                    "Subagent: {}",
                    if prompt.len() > 60 {
                        &prompt[..60]
                    } else {
                        prompt
                    }
                );
                let id = sm.spawn_subagent(desc, child, prompt.to_string());
                return Ok(json!({
                    "subagent_id": id.to_string(),
                    "status": "Running",
                    "mode": "background"
                })
                .to_string());
            } else {
                return Err("Background execution requested, but SubagentManager is not configured on AgentTool".into());
            }
        }

        // Synchronous execution: run child loop to completion and return response
        child.run(prompt).await
    }
}

// ---------------------------------------------------------------------------
// SendMessage Tool
// ---------------------------------------------------------------------------

pub struct SendMessageTool {
    task_manager: Option<TaskManager>,
    subagent_manager: Option<SubagentManager>,
}

impl Default for SendMessageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SendMessageTool {
    pub fn new() -> Self {
        Self {
            task_manager: None,
            subagent_manager: None,
        }
    }

    pub fn with_task_manager(mut self, task_manager: TaskManager) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    pub fn with_subagent_manager(mut self, subagent_manager: SubagentManager) -> Self {
        self.subagent_manager = Some(subagent_manager);
        self
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "SendMessage".into(),
            description: "Send input or messages to a running background task or subagent".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target_id": {
                        "type": "string",
                        "description": "ID of the background task or subagent to message"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message or input to send"
                    }
                },
                "required": ["target_id", "message"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let target_id = input["target_id"]
            .as_str()
            .or_else(|| input["task_id"].as_str())
            .or_else(|| input["subagent_id"].as_str())
            .or_else(|| input["recipient"].as_str())
            .ok_or("Missing 'target_id' argument")?;

        let message = input["message"]
            .as_str()
            .or_else(|| input["input"].as_str())
            .or_else(|| input["content"].as_str())
            .ok_or("Missing 'message' argument")?;

        let mut delivered = false;
        let mut task_err = None;
        let mut subagent_err = None;

        // Try task manager
        if let Some(ref tm) = self.task_manager {
            let task_id = crate::tasks::TaskId::from(target_id);
            match tm.send_input(&task_id, message) {
                Ok(()) => {
                    delivered = true;
                }
                Err(e) => {
                    task_err = Some(e);
                }
            }
        }

        // If not delivered, try subagent manager
        if !delivered {
            if let Some(ref sm) = self.subagent_manager {
                let sub_id = SubagentId::from(target_id);
                match sm.send_message(&sub_id, message) {
                    Ok(()) => {
                        delivered = true;
                    }
                    Err(e) => {
                        subagent_err = Some(e);
                    }
                }
            }
        }

        if delivered {
            Ok(format!("Message successfully delivered to '{target_id}'."))
        } else {
            let mut errs = Vec::new();
            if let Some(e) = task_err {
                errs.push(format!("Task delivery error: {e}"));
            }
            if let Some(e) = subagent_err {
                errs.push(format!("Subagent delivery error: {e}"));
            }
            if errs.is_empty() {
                Err(format!(
                    "Target '{target_id}' not found. Neither TaskManager nor SubagentManager matched."
                ))
            } else {
                Err(format!(
                    "Could not deliver to '{target_id}': {}",
                    errs.join("; ")
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        CompletionRequest, CompletionResponse, EventStream, ModelCapabilities, ProviderError,
        StreamEvent, TokenUsage,
    };
    use std::time::Duration;

    struct MockProvider {
        reply_text: String,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn id(&self) -> &str {
            "mock"
        }
        fn name(&self) -> &str {
            "Mock Provider"
        }
        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities::default()
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            Ok(CompletionResponse {
                message_id: "msg-1".into(),
                message: agent_core::Message::assistant_text(self.reply_text.clone()),
                usage: Some(TokenUsage::default()),
            })
        }
        async fn stream(&self, _req: CompletionRequest) -> Result<EventStream, ProviderError> {
            let events = vec![
                Ok(StreamEvent::MessageStarted {
                    message_id: "msg-1".into(),
                }),
                Ok(StreamEvent::TextDelta {
                    text: self.reply_text.clone(),
                }),
                Ok(StreamEvent::MessageCompleted {
                    usage: Some(TokenUsage::default()),
                }),
            ];
            let stream = futures::stream::iter(events);
            Ok(Box::pin(stream))
        }
    }

    #[tokio::test]
    async fn test_agent_tool_spawns_child_with_isolated_context_and_returns_response() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(MockProvider {
            reply_text: "Child agent completed sub-task successfully.".into(),
        });

        let policy = PermissionPolicy::default();
        let agent_tool = AgentTool::new(mock_provider, policy, &sessions_dir, "mock-model");

        let input = json!({
            "prompt": "Inspect workspace dependencies"
        });

        let response = agent_tool
            .execute(input, ws_root)
            .await
            .expect("Child agent execution should succeed");

        assert_eq!(response, "Child agent completed sub-task successfully.");

        // Verify child session files were written into sessions_dir
        assert!(sessions_dir.exists());
        let mut session_files = std::fs::read_dir(&sessions_dir).unwrap();
        assert!(session_files.next().is_some());
    }

    #[tokio::test]
    async fn test_agent_tool_with_custom_system_prompt_and_turns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(MockProvider {
            reply_text: "Custom child answer".into(),
        });

        let policy = PermissionPolicy::default();
        let agent_tool = AgentTool::new(mock_provider, policy, &sessions_dir, "mock-model");

        let input = json!({
            "prompt": "Analyze code",
            "system_prompt": "You are a specialized code reviewer.",
            "max_turns": 5
        });

        let res = agent_tool.execute(input, ws_root).await.unwrap();
        assert_eq!(res, "Custom child answer");
    }

    #[tokio::test]
    async fn test_send_message_tool_to_background_task() {
        let task_manager = TaskManager::new();

        let task_id =
            task_manager.create_task_with_context("Worker task", "worker", |ctx| async move {
                if let Some(msg) = ctx.recv_input().await {
                    ctx.log(&format!("Received input: {msg}"));
                    Ok(0)
                } else {
                    Err("No input".into())
                }
            });

        tokio::time::sleep(Duration::from_millis(20)).await;

        let send_tool = SendMessageTool::new().with_task_manager(task_manager.clone());

        let temp_dir = tempfile::tempdir().unwrap();
        let res = send_tool
            .execute(
                json!({
                    "target_id": task_id.to_string(),
                    "message": "start_job"
                }),
                temp_dir.path(),
            )
            .await
            .unwrap();

        assert!(res.contains("Message successfully delivered"));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let info = task_manager.get_task(&task_id).unwrap();
        assert!(info.output_buffer.contains("Received input: start_job"));
    }

    #[tokio::test]
    async fn test_send_message_tool_to_subagent() {
        let sub_manager = SubagentManager::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(MockProvider {
            reply_text: "Subagent initial reply".into(),
        });
        let child_loop = AgentLoop::new(
            mock_provider,
            ws_root,
            PermissionPolicy::default(),
            &sessions_dir,
            "mock-model",
        );

        let sub_id = sub_manager.spawn_subagent("Test child", child_loop, "Hello child".into());

        let send_tool = SendMessageTool::new().with_subagent_manager(sub_manager.clone());

        let res = send_tool
            .execute(
                json!({
                    "target_id": sub_id.to_string(),
                    "message": "follow up question"
                }),
                ws_root,
            )
            .await
            .unwrap();

        assert!(res.contains("Message successfully delivered"));

        let info = sub_manager.get_subagent(&sub_id).unwrap();
        assert_eq!(info.messages_delivered, 1);
    }

    #[tokio::test]
    async fn test_send_message_unknown_target() {
        let send_tool = SendMessageTool::new()
            .with_task_manager(TaskManager::new())
            .with_subagent_manager(SubagentManager::new());

        let temp_dir = tempfile::tempdir().unwrap();
        let res = send_tool
            .execute(
                json!({
                    "target_id": "non_existent_id",
                    "message": "ping"
                }),
                temp_dir.path(),
            )
            .await;

        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .contains("Could not deliver to 'non_existent_id'"));
    }

    #[tokio::test]
    async fn test_subagent_cancellation() {
        let sub_manager = SubagentManager::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(MockProvider {
            reply_text: "Done".into(),
        });
        let child_loop = AgentLoop::new(
            mock_provider,
            ws_root,
            PermissionPolicy::default(),
            &sessions_dir,
            "mock-model",
        );

        let sub_id = sub_manager.spawn_subagent("Long task", child_loop, "Run".into());

        sub_manager
            .stop_subagent(&sub_id)
            .expect("Stop should succeed");

        let info = sub_manager.get_subagent(&sub_id).unwrap();
        assert_eq!(info.status, SubagentStatus::Cancelled);
    }
}
