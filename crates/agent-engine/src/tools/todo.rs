use super::resolve_workspace_path;
use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
            TodoStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    #[serde(default)]
    pub id: Option<String>,
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub priority: Option<String>,
}

static SESSION_TODOS: RwLock<Vec<TodoItem>> = RwLock::const_new(Vec::new());

pub async fn get_session_todos() -> Vec<TodoItem> {
    SESSION_TODOS.read().await.clone()
}

pub async fn clear_session_todos() {
    let mut lock = SESSION_TODOS.write().await;
    lock.clear();
}

pub struct TodoWriteTool;

impl Default for TodoWriteTool {
    fn default() -> Self {
        Self
    }
}

impl TodoWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "TodoWrite".into(),
            description: "Update the session task checklist. Overwrites the active todo list with the provided items."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The updated list of all todo items for the session",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Optional unique identifier for the item"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Clear description of the task"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                                    "description": "Current status of the todo item"
                                },
                                "priority": {
                                    "type": "string",
                                    "enum": ["high", "medium", "low"],
                                    "description": "Optional priority level"
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let todos_raw = input["todos"]
            .as_array()
            .ok_or("Missing or invalid 'todos' array argument")?;

        let mut parsed_items = Vec::new();

        for (idx, val) in todos_raw.iter().enumerate() {
            let content = val["content"]
                .as_str()
                .ok_or_else(|| format!("Todo at index {idx} missing 'content'"))?
                .trim()
                .to_string();

            if content.is_empty() {
                return Err(format!("Todo at index {idx} has empty 'content'"));
            }

            let status_str = val["status"]
                .as_str()
                .ok_or_else(|| format!("Todo at index {idx} missing 'status'"))?;

            let status = match status_str {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                "cancelled" => TodoStatus::Cancelled,
                other => {
                    return Err(format!(
                        "Invalid status '{other}' at index {idx}. Expected 'pending', 'in_progress', 'completed', or 'cancelled'"
                    ))
                }
            };

            let id = val["id"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("todo-{}", idx + 1));

            let priority = val["priority"].as_str().map(|s| s.to_lowercase());

            parsed_items.push(TodoItem {
                id: Some(id),
                content,
                status,
                priority,
            });
        }

        // Persist to workspace if .agent or workspace path is accessible
        if let Ok(agent_dir) = resolve_workspace_path(workspace_root, ".agent") {
            let todos_file = agent_dir.join("todos.json");
            let _ = tokio::fs::create_dir_all(&agent_dir).await;
            if let Ok(serialized) = serde_json::to_string_pretty(&parsed_items) {
                let _ = tokio::fs::write(&todos_file, serialized).await;
            }
        }

        let mut completed_count = 0;
        let mut in_progress_count = 0;
        let mut pending_count = 0;
        let mut cancelled_count = 0;

        for item in &parsed_items {
            match item.status {
                TodoStatus::Completed => completed_count += 1,
                TodoStatus::InProgress => in_progress_count += 1,
                TodoStatus::Pending => pending_count += 1,
                TodoStatus::Cancelled => cancelled_count += 1,
            }
        }

        let mut output = format!(
            "Updated task checklist ({} total: {} completed, {} in progress, {} pending, {} cancelled):\n",
            parsed_items.len(),
            completed_count,
            in_progress_count,
            pending_count,
            cancelled_count
        );

        for item in &parsed_items {
            let symbol = match item.status {
                TodoStatus::Completed => "[x]",
                TodoStatus::InProgress => "[/]",
                TodoStatus::Pending => "[ ]",
                TodoStatus::Cancelled => "[-]",
            };
            let prio_info = item
                .priority
                .as_deref()
                .map(|p| format!(" (priority: {p})"))
                .unwrap_or_default();

            output.push_str(&format!("- {} {}{}\n", symbol, item.content, prio_info));
        }

        // Update in-memory session state
        {
            let mut lock = SESSION_TODOS.write().await;
            *lock = parsed_items;
        }

        Ok(output.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_todo_write_and_state_retrieval() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = TodoWriteTool;

        let input = json!({
            "todos": [
                {
                    "content": "Design architecture",
                    "status": "completed"
                },
                {
                    "content": "Write unit tests",
                    "status": "in_progress",
                    "priority": "high"
                },
                {
                    "content": "Deploy release",
                    "status": "pending"
                }
            ]
        });

        let res = tool.execute(input, root).await.unwrap();
        assert!(res.contains(
            "Updated task checklist (3 total: 1 completed, 1 in progress, 1 pending, 0 cancelled)"
        ));
        assert!(res.contains("- [x] Design architecture"));
        assert!(res.contains("- [/] Write unit tests (priority: high)"));
        assert!(res.contains("- [ ] Deploy release"));

        let items = get_session_todos().await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].status, TodoStatus::Completed);
        assert_eq!(items[1].status, TodoStatus::InProgress);
        assert_eq!(items[1].priority.as_deref(), Some("high"));
        assert_eq!(items[2].status, TodoStatus::Pending);

        // Verify .agent/todos.json was written
        let saved_file = root.join(".agent").join("todos.json");
        assert!(saved_file.exists());
        let content = tokio::fs::read_to_string(&saved_file).await.unwrap();
        let saved_todos: Vec<TodoItem> = serde_json::from_str(&content).unwrap();
        assert_eq!(saved_todos.len(), 3);
    }

    #[tokio::test]
    async fn test_todo_write_invalid_status() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = TodoWriteTool;

        let input = json!({
            "todos": [
                {
                    "content": "Invalid status test",
                    "status": "not_a_valid_status"
                }
            ]
        });

        let res = tool.execute(input, root).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Invalid status"));
    }
}
