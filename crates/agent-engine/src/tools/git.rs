use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// GitStatus Tool
// ---------------------------------------------------------------------------
pub struct GitStatusTool;

impl GitStatusTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "GitStatus".into(),
            description: "Inspect git repository status, current branch, staged/unstaged changes, and untracked files without executing a subshell.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Optional subdirectory or file path to scope git status to"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.args(["status", "--porcelain=v2", "--branch"]);
        cmd.current_dir(workspace_root);

        if let Some(target) = input.get("path").and_then(|v| v.as_str()) {
            if !target.trim().is_empty() {
                cmd.arg("--");
                cmd.arg(target.trim());
            }
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to execute git status: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "git status exited with error: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if stdout.trim().is_empty() {
            Ok("Clean working tree (no modifications).".into())
        } else {
            Ok(stdout)
        }
    }
}

// ---------------------------------------------------------------------------
// GitDiff Tool
// ---------------------------------------------------------------------------
pub struct GitDiffTool;

impl GitDiffTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitDiffTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "GitDiff".into(),
            description: "Inspect git diff of unstaged working tree changes, staged changes, or differences against a branch or commit hash.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "staged": {
                        "type": "boolean",
                        "description": "If true, show staged changes (--cached)"
                    },
                    "revision": {
                        "type": "string",
                        "description": "Optional branch, tag, or commit to compare against (e.g. 'HEAD~1', 'main')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional path to scope the diff to"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.arg("diff");
        cmd.current_dir(workspace_root);

        if input
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            cmd.arg("--cached");
        }

        if let Some(rev) = input.get("revision").and_then(|v| v.as_str()) {
            if !rev.trim().is_empty() {
                cmd.arg(rev.trim());
            }
        }

        if let Some(target) = input.get("path").and_then(|v| v.as_str()) {
            if !target.trim().is_empty() {
                cmd.arg("--");
                cmd.arg(target.trim());
            }
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to execute git diff: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "git diff exited with error: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if stdout.trim().is_empty() {
            Ok("No differences found.".into())
        } else {
            Ok(stdout)
        }
    }
}

// ---------------------------------------------------------------------------
// GitCommit Tool
// ---------------------------------------------------------------------------
pub struct GitCommitTool;

impl GitCommitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitCommitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "GitCommit".into(),
            description: "Stage changes and create a Git commit with a descriptive message.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["message"],
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Descriptive commit message"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Specific files to stage before committing"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "If true, stage all modified and untracked files (git add -A) before committing"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let message = input
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'message'".to_string())?
            .trim();

        if message.is_empty() {
            return Err("Commit message cannot be empty".into());
        }

        let all = input.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
        let files = input.get("files").and_then(|v| v.as_array());

        // Stage files
        if all {
            let add_status = Command::new("git")
                .args(["add", "-A"])
                .current_dir(workspace_root)
                .status()
                .map_err(|e| format!("Failed to stage files with git add -A: {e}"))?;

            if !add_status.success() {
                return Err("Failed to stage all files".into());
            }
        } else if let Some(file_list) = files {
            if !file_list.is_empty() {
                let mut add_cmd = Command::new("git");
                add_cmd.arg("add");
                add_cmd.current_dir(workspace_root);
                for f in file_list {
                    if let Some(s) = f.as_str() {
                        add_cmd.arg(s);
                    }
                }
                let add_status = add_cmd
                    .status()
                    .map_err(|e| format!("Failed to stage specified files: {e}"))?;

                if !add_status.success() {
                    return Err("Failed to stage specified files".into());
                }
            }
        }

        // Commit
        let commit_output = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(workspace_root)
            .output()
            .map_err(|e| format!("Failed to execute git commit: {e}"))?;

        if !commit_output.status.success() {
            return Err(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&commit_output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&commit_output.stdout).to_string())
    }
}
