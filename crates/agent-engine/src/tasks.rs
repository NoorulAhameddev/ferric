use crate::tools::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Task Identifiers and Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for TaskId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for TaskId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed { exit_code: i32 },
    Failed { error: String },
    Cancelled,
}

impl TaskStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, TaskStatus::Running)
    }

    pub fn is_terminal(&self) -> bool {
        !self.is_running()
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Running => write!(f, "Running"),
            TaskStatus::Completed { exit_code } => write!(f, "Completed (exit code {exit_code})"),
            TaskStatus::Failed { error } => write!(f, "Failed: {error}"),
            TaskStatus::Cancelled => write!(f, "Cancelled"),
        }
    }
}

// ---------------------------------------------------------------------------
// Task Information
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: TaskId,
    pub description: String,
    pub tool_name: String,
    pub start_time: DateTime<Utc>,
    pub status: TaskStatus,
    pub output_buffer: String,
}

// ---------------------------------------------------------------------------
// Task Execution Context
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TaskContext {
    pub id: TaskId,
    pub token: CancellationToken,
    tasks: Arc<Mutex<HashMap<TaskId, TaskInfo>>>,
    input_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
}

pub const MAX_TASK_OUTPUT_BYTES: usize = 1_048_576; // 1 MB circular buffer limit
pub const RETAIN_TAIL_BYTES: usize = 524_288; // 512 KB retained tail

impl TaskContext {
    pub fn log(&self, text: &str) {
        if let Ok(mut tasks) = self.tasks.lock() {
            if let Some(info) = tasks.get_mut(&self.id) {
                info.output_buffer.push_str(text);
                if info.output_buffer.len() > MAX_TASK_OUTPUT_BYTES {
                    let cut_target = info.output_buffer.len().saturating_sub(RETAIN_TAIL_BYTES);
                    // Find valid utf8 char boundary
                    let mut start_idx = cut_target;
                    while start_idx < info.output_buffer.len()
                        && !info.output_buffer.is_char_boundary(start_idx)
                    {
                        start_idx += 1;
                    }
                    let tail = info.output_buffer[start_idx..].to_string();
                    info.output_buffer =
                        format!("[... earlier task output truncated ...]\n{}", tail);
                }
            }
        }
    }

    pub fn append_output(&self, text: &str) {
        self.log(text);
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn recv_input(&self) -> Option<String> {
        let mut rx = self.input_rx.lock().await;
        rx.recv().await
    }

    pub fn id(&self) -> &TaskId {
        &self.id
    }

    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

// ---------------------------------------------------------------------------
// Task Controller (Internal)
// ---------------------------------------------------------------------------

struct TaskController {
    cancel_token: CancellationToken,
    input_tx: mpsc::Sender<String>,
}

// ---------------------------------------------------------------------------
// Task Manager
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<TaskId, TaskInfo>>>,
    controllers: Arc<Mutex<HashMap<TaskId, TaskController>>>,
    counter: Arc<AtomicU64>,
}

pub type TaskTracker = TaskManager;

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            controllers: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn tasks(&self) -> &Arc<Mutex<HashMap<TaskId, TaskInfo>>> {
        &self.tasks
    }

    /// Spawns a background task running an arbitrary future with a cancellation token.
    pub fn create_task<Fut>(
        &self,
        description: impl Into<String>,
        tool_name: impl Into<String>,
        future: Fut,
    ) -> TaskId
    where
        Fut: Future<Output = Result<i32, String>> + Send + 'static,
    {
        self.create_task_with_context(description, tool_name, |_ctx| future)
    }

    /// Spawns a background task with full `TaskContext` access (logging, messages, cancellation).
    pub fn create_task_with_context<F, Fut>(
        &self,
        description: impl Into<String>,
        tool_name: impl Into<String>,
        f: F,
    ) -> TaskId
    where
        F: FnOnce(TaskContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<i32, String>> + Send + 'static,
    {
        let id_num = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = TaskId::new(format!("task-{id_num}"));
        let token = CancellationToken::new();
        let (input_tx, input_rx) = mpsc::channel(64);

        let info = TaskInfo {
            id: id.clone(),
            description: description.into(),
            tool_name: tool_name.into(),
            start_time: Utc::now(),
            status: TaskStatus::Running,
            output_buffer: String::new(),
        };

        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(id.clone(), info);
        }

        if let Ok(mut controllers) = self.controllers.lock() {
            controllers.insert(
                id.clone(),
                TaskController {
                    cancel_token: token.clone(),
                    input_tx,
                },
            );
        }

        let ctx = TaskContext {
            id: id.clone(),
            token: token.clone(),
            tasks: self.tasks.clone(),
            input_rx: Arc::new(tokio::sync::Mutex::new(input_rx)),
        };

        let tasks_clone = self.tasks.clone();
        let task_id_clone = id.clone();
        let token_clone = token.clone();

        tokio::spawn(async move {
            let fut = f(ctx);
            tokio::select! {
                _ = token_clone.cancelled() => {
                    if let Ok(mut tasks) = tasks_clone.lock() {
                        if let Some(info) = tasks.get_mut(&task_id_clone) {
                            if info.status == TaskStatus::Running {
                                info.status = TaskStatus::Cancelled;
                            }
                        }
                    }
                }
                res = fut => {
                    if let Ok(mut tasks) = tasks_clone.lock() {
                        if let Some(info) = tasks.get_mut(&task_id_clone) {
                            if info.status == TaskStatus::Running {
                                match res {
                                    Ok(code) => {
                                        info.status = TaskStatus::Completed { exit_code: code };
                                    }
                                    Err(err) => {
                                        info.status = TaskStatus::Failed { error: err };
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

    /// Spawns an external shell command as a background task with live output streaming.
    pub fn spawn_command(
        &self,
        command: &str,
        description: Option<&str>,
        workspace_root: &Path,
    ) -> TaskId {
        let cmd_str = command.to_string();
        let root = workspace_root.to_path_buf();
        let desc = description.unwrap_or(command).to_string();

        self.create_task_with_context(desc, "shell", move |ctx| async move {
            let mut cmd = if cfg!(target_os = "windows") {
                let mut c = tokio::process::Command::new("powershell.exe");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &cmd_str]);
                c
            } else {
                let mut c = tokio::process::Command::new("bash");
                c.args(["-c", &cmd_str]);
                c
            };

            cmd.current_dir(&root);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.stdin(std::process::Stdio::piped());
            cmd.kill_on_drop(true);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return Err(format!("Failed to spawn command: {e}")),
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stdin = child.stdin.take();

            // Stream stdout
            let ctx_out = ctx.clone();
            if let Some(out) = stdout {
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut reader = tokio::io::BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        ctx_out.log(&format!("{line}\n"));
                    }
                });
            }

            // Stream stderr
            let ctx_err = ctx.clone();
            if let Some(err) = stderr {
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut reader = tokio::io::BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        ctx_err.log(&format!("{line}\n"));
                    }
                });
            }

            // Pipe input from ctx.recv_input() to stdin
            let ctx_in = ctx.clone();
            if let Some(mut sin) = stdin {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    while let Some(msg) = ctx_in.recv_input().await {
                        let _ = sin.write_all(msg.as_bytes()).await;
                        if !msg.ends_with('\n') {
                            let _ = sin.write_all(b"\n").await;
                        }
                        let _ = sin.flush().await;
                    }
                });
            }

            // Await completion or cancellation
            tokio::select! {
                _ = ctx.token().cancelled() => {
                    let _ = child.kill().await;
                    ctx.log("\n[Task cancelled by token]\n");
                    Ok(130)
                }
                status_res = child.wait() => {
                    match status_res {
                        Ok(status) => Ok(status.code().unwrap_or(0)),
                        Err(e) => Err(format!("Process error: {e}")),
                    }
                }
            }
        })
    }

    /// Returns a list of all known tasks sorted by start time.
    pub fn list_tasks(&self) -> Vec<TaskInfo> {
        if let Ok(tasks) = self.tasks.lock() {
            let mut list: Vec<TaskInfo> = tasks.values().cloned().collect();
            list.sort_by_key(|a| a.start_time);
            list
        } else {
            Vec::new()
        }
    }

    /// Fetches a task's current info if found.
    pub fn get_task(&self, id: &TaskId) -> Option<TaskInfo> {
        let tasks = self.tasks.lock().ok()?;
        tasks.get(id).cloned()
    }

    /// Stops/cancels a running task.
    pub fn stop_task(&self, id: &TaskId) -> Result<(), String> {
        let cancel_token = {
            let controllers = self
                .controllers
                .lock()
                .map_err(|e| format!("Lock error: {e}"))?;
            controllers
                .get(id)
                .map(|c| c.cancel_token.clone())
                .ok_or_else(|| format!("Task '{id}' not found"))?
        };

        cancel_token.cancel();

        if let Ok(mut tasks) = self.tasks.lock() {
            if let Some(info) = tasks.get_mut(id) {
                if info.status == TaskStatus::Running {
                    info.status = TaskStatus::Cancelled;
                    info.output_buffer.push_str("\n[Task cancelled by user]");
                }
            }
        }

        Ok(())
    }

    /// Sends input or message to a running task.
    pub fn send_input(&self, id: &TaskId, input: &str) -> Result<(), String> {
        let sender = {
            let controllers = self
                .controllers
                .lock()
                .map_err(|e| format!("Lock error: {e}"))?;
            controllers
                .get(id)
                .map(|c| c.input_tx.clone())
                .ok_or_else(|| format!("Task '{id}' not found"))?
        };

        sender
            .try_send(input.to_string())
            .map_err(|e| format!("Failed to deliver input to task '{id}': {e}"))
    }

    /// Appends output to a task's buffer directly.
    pub fn log_task_output(&self, id: &TaskId, text: &str) {
        if let Ok(mut tasks) = self.tasks.lock() {
            if let Some(info) = tasks.get_mut(id) {
                info.output_buffer.push_str(text);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent Tools for Tasks
// ---------------------------------------------------------------------------

pub struct TaskCreateTool {
    manager: TaskManager,
}

impl TaskCreateTool {
    pub fn new(manager: TaskManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "TaskCreate".into(),
            description:
                "Spawn a shell command as a background task with managed lifecycle and logging"
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute in background" },
                    "description": { "type": "string", "description": "Optional human-readable description of the task" },
                    "tool_name": { "type": "string", "description": "Optional tool name (defaults to 'shell')" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let command = input["command"]
            .as_str()
            .ok_or("Missing 'command' argument")?;
        let description = input["description"].as_str().unwrap_or(command);
        let tool_name = input["tool_name"].as_str().unwrap_or("shell");

        let task_id = self
            .manager
            .spawn_command(command, Some(description), workspace_root);

        // Update tool_name if custom provided
        if tool_name != "shell" {
            if let Ok(mut tasks) = self.manager.tasks.lock() {
                if let Some(info) = tasks.get_mut(&task_id) {
                    info.tool_name = tool_name.to_string();
                }
            }
        }

        Ok(json!({
            "task_id": task_id.to_string(),
            "status": "Running",
            "description": description,
            "tool_name": tool_name
        })
        .to_string())
    }
}

pub struct TaskListTool {
    manager: TaskManager,
}

impl TaskListTool {
    pub fn new(manager: TaskManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "TaskList".into(),
            description: "List all background tasks with their IDs, statuses, and metadata".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let tasks = self.manager.list_tasks();
        if tasks.is_empty() {
            return Ok("No background tasks found.".into());
        }
        serde_json::to_string_pretty(&tasks)
            .map_err(|e| format!("Failed to serialize task list: {e}"))
    }
}

pub struct TaskGetTool {
    manager: TaskManager,
}

impl TaskGetTool {
    pub fn new(manager: TaskManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskGetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "TaskGet".into(),
            description:
                "Retrieve status, metadata, and output logs for a specific background task".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "The task ID to fetch" },
                    "tail_lines": { "type": "integer", "description": "Optional number of recent output lines to tail" }
                },
                "required": ["task_id"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let task_id_str = input["task_id"]
            .as_str()
            .or_else(|| input["id"].as_str())
            .ok_or("Missing 'task_id' argument")?;
        let task_id = TaskId::from(task_id_str);

        let task = self
            .manager
            .get_task(&task_id)
            .ok_or_else(|| format!("Task '{task_id_str}' not found"))?;

        let tail_lines = input["tail_lines"].as_u64().map(|n| n as usize);
        let output = if let Some(n) = tail_lines {
            let lines: Vec<&str> = task.output_buffer.lines().collect();
            if lines.len() > n {
                lines[lines.len() - n..].join("\n")
            } else {
                task.output_buffer.clone()
            }
        } else {
            task.output_buffer.clone()
        };

        Ok(json!({
            "id": task.id.to_string(),
            "description": task.description,
            "tool_name": task.tool_name,
            "status": task.status,
            "start_time": task.start_time.to_rfc3339(),
            "output": output
        })
        .to_string())
    }
}

pub struct TaskStopTool {
    manager: TaskManager,
}

impl TaskStopTool {
    pub fn new(manager: TaskManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TaskStopTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "TaskStop".into(),
            description: "Stop or cancel a running background task".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "The task ID to stop" }
                },
                "required": ["task_id"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let task_id_str = input["task_id"]
            .as_str()
            .or_else(|| input["id"].as_str())
            .ok_or("Missing 'task_id' argument")?;
        let task_id = TaskId::from(task_id_str);

        self.manager.stop_task(&task_id)?;
        Ok(format!("Task '{task_id_str}' stopped successfully."))
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_task_lifecycle_complete_and_output() {
        let manager = TaskManager::new();

        let task_id =
            manager.create_task_with_context("Compile build", "build_tool", |ctx| async move {
                ctx.log("Compiling package...\n");
                tokio::time::sleep(Duration::from_millis(50)).await;
                ctx.log("Finished in 0.05s\n");
                Ok(0)
            });

        // Verify task exists and is initially Running
        let initial_info = manager.get_task(&task_id).expect("Task should exist");
        assert_eq!(initial_info.description, "Compile build");
        assert_eq!(initial_info.tool_name, "build_tool");
        assert!(initial_info.status.is_running());

        // Wait for completion
        tokio::time::sleep(Duration::from_millis(150)).await;

        let completed_info = manager.get_task(&task_id).expect("Task should exist");
        assert_eq!(
            completed_info.status,
            TaskStatus::Completed { exit_code: 0 }
        );
        assert!(completed_info
            .output_buffer
            .contains("Compiling package..."));
        assert!(completed_info.output_buffer.contains("Finished in 0.05s"));
    }

    #[tokio::test]
    async fn test_task_failure() {
        let manager = TaskManager::new();

        let task_id = manager.create_task("Faulty task", "test_tool", async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Err("Disk write failure".to_string())
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let task = manager.get_task(&task_id).expect("Task should exist");
        match task.status {
            TaskStatus::Failed { ref error } => {
                assert!(error.contains("Disk write failure"));
            }
            other => panic!("Expected Failed status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_task_cancellation_via_token() {
        let manager = TaskManager::new();

        let task_id =
            manager.create_task_with_context("Long running watcher", "watcher", |ctx| async move {
                ctx.log("Watcher started\n");
                while !ctx.is_cancelled() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                ctx.log("Watcher aborted\n");
                Ok(130)
            });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify Running
        let running_info = manager.get_task(&task_id).unwrap();
        assert!(running_info.status.is_running());

        // Cancel task
        manager.stop_task(&task_id).expect("Stop should succeed");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let cancelled_info = manager.get_task(&task_id).unwrap();
        assert_eq!(cancelled_info.status, TaskStatus::Cancelled);
        assert!(cancelled_info.output_buffer.contains("Watcher started"));
    }

    #[tokio::test]
    async fn test_task_listing_and_retrieval() {
        let manager = TaskManager::new();

        let id1 = manager.create_task("Task 1", "tool", async { Ok(0) });
        let id2 = manager.create_task("Task 2", "tool", async { Ok(0) });

        let list = manager.list_tasks();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|t| t.id == id1));
        assert!(list.iter().any(|t| t.id == id2));

        let task = manager.get_task(&id1);
        assert!(task.is_some());
        assert_eq!(task.unwrap().description, "Task 1");

        let unknown = manager.get_task(&TaskId::from("non-existent"));
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn test_task_messaging_and_input() {
        let manager = TaskManager::new();

        let task_id =
            manager.create_task_with_context("Interactive task", "repl", |ctx| async move {
                ctx.log("Awaiting input...\n");
                if let Some(msg) = ctx.recv_input().await {
                    ctx.log(&format!("Received: {msg}\n"));
                    Ok(0)
                } else {
                    Err("No input received".into())
                }
            });

        tokio::time::sleep(Duration::from_millis(30)).await;

        manager
            .send_input(&task_id, "hello world")
            .expect("Send input should succeed");

        tokio::time::sleep(Duration::from_millis(80)).await;

        let info = manager.get_task(&task_id).unwrap();
        assert_eq!(info.status, TaskStatus::Completed { exit_code: 0 });
        assert!(info.output_buffer.contains("Received: hello world"));
    }

    #[tokio::test]
    async fn test_task_tools_execution() {
        let manager = TaskManager::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();

        let create_tool = TaskCreateTool::new(manager.clone());
        let list_tool = TaskListTool::new(manager.clone());
        let get_tool = TaskGetTool::new(manager.clone());
        let stop_tool = TaskStopTool::new(manager.clone());

        // 1. TaskList when empty
        let empty_list = list_tool.execute(json!({}), ws_root).await.unwrap();
        assert!(empty_list.contains("No background tasks found"));

        // 2. TaskCreate
        let create_res = create_tool
            .execute(
                json!({
                    "command": if cfg!(windows) { "cmd.exe /c echo hello" } else { "echo hello" },
                    "description": "Echo test"
                }),
                ws_root,
            )
            .await
            .unwrap();
        let created_val: serde_json::Value = serde_json::from_str(&create_res).unwrap();
        let task_id = created_val["task_id"].as_str().unwrap();

        // 3. TaskList has 1 task
        let list_res = list_tool.execute(json!({}), ws_root).await.unwrap();
        assert!(list_res.contains(task_id));

        // 4. TaskGet fetches task info
        let get_res = get_tool
            .execute(json!({ "task_id": task_id }), ws_root)
            .await
            .unwrap();
        assert!(get_res.contains(task_id));

        // 5. TaskStop stops task
        let stop_res = stop_tool
            .execute(json!({ "task_id": task_id }), ws_root)
            .await
            .unwrap();
        assert!(stop_res.contains("stopped successfully"));

        // Verify task status is Cancelled
        let task_info = manager.get_task(&TaskId::from(task_id)).unwrap();
        assert!(task_info.status == TaskStatus::Cancelled || task_info.status.is_terminal());
    }
}
