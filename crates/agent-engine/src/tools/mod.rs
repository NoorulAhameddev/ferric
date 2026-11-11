pub mod git;
pub mod notebook;
pub mod patch;
pub mod plan;
pub mod question;
pub mod skill;
pub mod todo;
pub mod web;

pub use git::{GitCommitTool, GitDiffTool, GitStatusTool};
pub use notebook::NotebookEditTool;
pub use patch::{parse_unified_diff, PatchApplyTool};
pub use plan::{
    get_finalized_plan, get_plan_reason, is_plan_mode_active, plan_mode_tools, reset_plan_mode,
    set_plan_mode_active, EnterPlanModeTool, ExitPlanModeTool, PlanModeState, PlanModeTool,
};
pub use question::{
    clear_question_handler, set_question_handler, AskUserQuestionTool, QuestionHandler,
    QuestionOption,
};
pub use skill::{SkillInfo, SkillTool};
pub use todo::{clear_session_todos, get_session_todos, TodoItem, TodoStatus, TodoWriteTool};
pub use web::{
    html_to_markdown, set_search_provider_override, DuckDuckGoSearchProvider, SearchProvider,
    SearchResult, WebFetchTool, WebSearchTool,
};

use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// FileRead Tool
// ---------------------------------------------------------------------------
pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "FileRead".into(),
            description: "Read contents of a file at the given path".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Relative or absolute path to the file" },
                    "offset": { "type": "integer", "description": "Optional line offset to begin reading" },
                    "limit": { "type": "integer", "description": "Optional maximum lines to read" }
                },
                "required": ["file_path"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let file_path_str = input["file_path"]
            .as_str()
            .ok_or("Missing 'file_path' argument")?;
        let full_path = resolve_workspace_path(workspace_root, file_path_str)?;

        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {e}", full_path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let limit = input["limit"].as_u64().unwrap_or(lines.len() as u64) as usize;

        if offset >= lines.len() && !lines.is_empty() {
            return Ok(String::new());
        }

        let slice = &lines[offset..lines.len().min(offset + limit)];
        Ok(slice.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// FileWrite Tool
// ---------------------------------------------------------------------------
pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "FileWrite".into(),
            description: "Write content to a file, creating parent directories if needed".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Relative path to write to" },
                    "content": { "type": "string", "description": "Full file content" }
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let file_path_str = input["file_path"]
            .as_str()
            .ok_or("Missing 'file_path' argument")?;
        let content = input["content"]
            .as_str()
            .ok_or("Missing 'content' argument")?;
        let full_path = resolve_workspace_path(workspace_root, file_path_str)?;

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directory: {e}"))?;
        }

        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| format!("Failed to write file '{}': {e}", full_path.display()))?;

        Ok(format!(
            "Successfully wrote {} bytes to '{}'",
            content.len(),
            file_path_str
        ))
    }
}

// ---------------------------------------------------------------------------
// FileEdit Tool
// ---------------------------------------------------------------------------
pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "FileEdit".into(),
            description: "Perform an exact string replacement in a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the file to edit" },
                    "target": { "type": "string", "description": "Exact text to replace" },
                    "replacement": { "type": "string", "description": "New replacement text" }
                },
                "required": ["file_path", "target", "replacement"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let file_path_str = input["file_path"].as_str().ok_or("Missing 'file_path'")?;
        let target = input["target"].as_str().ok_or("Missing 'target'")?;
        let replacement = input["replacement"]
            .as_str()
            .ok_or("Missing 'replacement'")?;
        let full_path = resolve_workspace_path(workspace_root, file_path_str)?;

        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| format!("Failed to read file: {e}"))?;

        if !content.contains(target) {
            return Err(format!("Target string not found in '{}'", file_path_str));
        }

        let new_content = content.replace(target, replacement);
        tokio::fs::write(&full_path, new_content)
            .await
            .map_err(|e| format!("Failed to write modified file: {e}"))?;

        Ok(format!("Successfully edited '{}'", file_path_str))
    }
}

// ---------------------------------------------------------------------------
// Glob Tool
// ---------------------------------------------------------------------------
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Glob".into(),
            description: "Find files matching a glob pattern (e.g. '**/*.rs', 'src/**/*.ts')"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern to match" },
                    "path": { "type": "string", "description": "Base directory to search in (relative to workspace root)" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or("Missing 'pattern' argument")?;
        let base_rel = input["path"].as_str().unwrap_or(".");
        let base_dir = resolve_workspace_path(workspace_root, base_rel)?;

        let mut matches = Vec::new();
        collect_glob_matches(&base_dir, &base_dir, pattern, &mut matches, 0)?;
        matches.sort();

        if matches.is_empty() {
            Ok("No matching files found".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

fn collect_glob_matches(
    current: &Path,
    root: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth > 20 {
        return Ok(());
    }

    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if !is_ignored_dir(&file_name) {
                collect_glob_matches(&path, root, pattern, matches, depth + 1)?;
            }
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let norm_rel = rel.to_string_lossy().replace('\\', "/");
            if crate::permissions::wildcard_match(pattern, &norm_rel)
                || crate::permissions::wildcard_match(pattern, &file_name)
            {
                matches.push(norm_rel);
            }
        }
    }

    Ok(())
}

fn is_ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".xwin" | ".cargo" | ".gemini" | "build" | "dist"
    )
}

// ---------------------------------------------------------------------------
// Grep Tool
// ---------------------------------------------------------------------------
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Grep".into(),
            description: "Search file contents across the workspace using regular expressions"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "File or directory to search in" },
                    "case_sensitive": { "type": "boolean", "description": "Whether search is case-sensitive (default: true)" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let pattern_str = input["pattern"]
            .as_str()
            .ok_or("Missing 'pattern' argument")?;
        let path_str = input["path"].as_str().unwrap_or(".");
        let case_sensitive = input["case_sensitive"].as_bool().unwrap_or(true);
        let target_path = resolve_workspace_path(workspace_root, path_str)?;

        let regex = regex::RegexBuilder::new(pattern_str)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("Invalid regex pattern: {e}"))?;

        let mut results = Vec::new();
        search_grep_path(&target_path, workspace_root, &regex, &mut results, 0)?;

        if results.is_empty() {
            Ok("No matching lines found".to_string())
        } else {
            let total = results.len();
            if total > 500 {
                results.truncate(500);
                results.push(format!(
                    "\n... [truncated {} additional matches]",
                    total - 500
                ));
            }
            Ok(results.join("\n"))
        }
    }
}

fn search_grep_path(
    current: &Path,
    root: &Path,
    regex: &regex::Regex,
    results: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth > 20 || results.len() > 1000 {
        return Ok(());
    }

    if current.is_file() {
        search_grep_file(current, root, regex, results)?;
        return Ok(());
    }

    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if !is_ignored_dir(&file_name) {
                search_grep_path(&path, root, regex, results, depth + 1)?;
            }
        } else {
            search_grep_file(&path, root, regex, results)?;
        }
    }

    Ok(())
}

fn search_grep_file(
    file_path: &Path,
    root: &Path,
    regex: &regex::Regex,
    results: &mut Vec<String>,
) -> Result<(), String> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    for (line_num, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            results.push(format!("{}:{}: {}", rel_str, line_num + 1, line.trim_end()));
            if results.len() > 1000 {
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Shell Execution Tool (PowerShell on Windows, Bash on Unix)
// ---------------------------------------------------------------------------
pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Bash".into(),
            description: "Execute a shell command with timeout and process tree management".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to run" },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds" }
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
        let command_str = input["command"]
            .as_str()
            .ok_or("Missing 'command' argument")?;
        let timeout_ms = input["timeout_ms"].as_u64().unwrap_or(30_000);

        #[cfg(target_os = "windows")]
        use std::os::windows::process::CommandExt;

        #[cfg(target_os = "windows")]
        let mut cmd = tokio::process::Command::new("powershell.exe");
        #[cfg(target_os = "windows")]
        {
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", command_str]);
            // CREATE_SUSPENDED = 0x00000004 guarantees the child cannot spawn grandchildren before being bound to the Job Object
            cmd.as_std_mut().creation_flags(0x00000004);
        }

        #[cfg(not(target_os = "windows"))]
        let mut cmd = tokio::process::Command::new("bash");
        #[cfg(not(target_os = "windows"))]
        cmd.args(["-c", command_str]);

        cmd.current_dir(workspace_root);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        #[cfg(target_os = "windows")]
        let job = win_job::JobObjectGuard::create()
            .map_err(|e| format!("Failed to create Job Object: {e}"))?;

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn process: {e}"))?;

        #[cfg(target_os = "windows")]
        {
            if let Some(raw_handle) = child.raw_handle() {
                let h = raw_handle as windows_sys::Win32::Foundation::HANDLE;
                if let Err(e) = job.assign_handle(h) {
                    tracing::error!("Failed to assign process to Job Object: {e}");
                    unsafe {
                        windows_sys::Win32::System::Threading::TerminateProcess(h, 1);
                    }
                    return Err(format!("Process security initialization failed: {e}"));
                }
                if let Err(e) = job.resume_handle(h) {
                    tracing::error!("Failed to resume process: {e}");
                    unsafe {
                        windows_sys::Win32::System::Threading::TerminateProcess(h, 1);
                    }
                    return Err(format!("Process security initialization failed: {e}"));
                }
            } else {
                return Err("Failed to retrieve process handle for Job Object binding".to_string());
            }
        }

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let output = tokio::time::timeout(timeout_duration, child.wait_with_output())
            .await
            .map_err(|_| format!("Command timed out after {timeout_ms}ms"))?
            .map_err(|e| format!("Process error: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

        if code == 0 {
            Ok(stdout.to_string())
        } else {
            Err(format!("Exit code {code}:\n{stdout}\n{stderr}"))
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) mod win_job {
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    // ARCHITECTURAL RATIONALE FOR NtResumeProcess:
    // When spawning child processes under Windows with CREATE_SUSPENDED (0x00000004) to prevent
    // race conditions (e.g. child spawning uncontrolled grandchild processes before Job Object assignment),
    // the parent process must resume the suspended process.
    //
    // Under standard Win32 API, resuming requires the primary thread handle (`ResumeThread`). However,
    // Rust's standard library (`std::process::Command` / `tokio::process::Command`) encapsulates
    // `PROCESS_INFORMATION` internally, exposing only `raw_handle()` which yields `hProcess` (process handle).
    // The thread handle `hThread` is private and inaccessible.
    //
    // To resume all threads belonging to `hProcess` without reimplementing the full `CreateProcessW`
    // lifecycle from scratch in user-space, `NtResumeProcess` from `ntdll.dll` is the standard NT kernel
    // system call (present and backward-compatible across Windows NT from 2000 through Windows 11).
    //
    // Process Handle Ownership & Lifecycle Safety:
    // 1. Ownership: `child.raw_handle()` returns a borrowed HANDLE whose ownership remains with `tokio::process::Child`.
    // 2. Failure Cleanliness: If `AssignProcessToJobObject` fails, `TerminateProcess(h, 1)` is called immediately
    //    before returning an error, guaranteeing that no suspended child processes leak into the OS.
    // 3. Sandboxing Limits: Windows Job Objects provide process tree lifetime enforcement (`KILL_ON_JOB_CLOSE`)
    //    and resource quotas; they do not provide filesystem/network AppContainer sandboxing.
    extern "system" {
        fn NtResumeProcess(process_handle: HANDLE) -> i32;
    }

    pub struct JobObjectGuard(HANDLE);

    unsafe impl Send for JobObjectGuard {}
    unsafe impl Sync for JobObjectGuard {}

    impl JobObjectGuard {
        pub fn create() -> Result<Self, String> {
            unsafe {
                let job = CreateJobObjectW(null(), null());
                if job.is_null() || job == INVALID_HANDLE_VALUE {
                    return Err("Failed to create Win32 Job Object".to_string());
                }

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );

                if ok == 0 {
                    CloseHandle(job);
                    return Err("Failed to set Job Object limits".to_string());
                }

                Ok(Self(job))
            }
        }

        pub fn assign_handle(&self, handle: HANDLE) -> Result<(), String> {
            unsafe {
                let ok = AssignProcessToJobObject(self.0, handle);
                if ok == 0 {
                    return Err("Failed to assign process handle to Job Object".to_string());
                }
                Ok(())
            }
        }

        pub fn resume_handle(&self, handle: HANDLE) -> Result<(), String> {
            unsafe {
                let status = NtResumeProcess(handle);
                if status < 0 {
                    return Err(format!("NtResumeProcess failed with status: {status:#x}"));
                }
                Ok(())
            }
        }

        #[allow(dead_code)]
        pub fn assign_process(&self, pid: u32) -> Result<(), String> {
            unsafe {
                let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if proc.is_null() || proc == INVALID_HANDLE_VALUE {
                    return Err(format!("Failed to open process {pid}"));
                }

                let ok = AssignProcessToJobObject(self.0, proc);
                CloseHandle(proc);

                if ok == 0 {
                    return Err(format!("Failed to assign process {pid} to Job Object"));
                }
                Ok(())
            }
        }
    }

    impl Drop for JobObjectGuard {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                    CloseHandle(self.0);
                }
            }
        }
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(p) => components.push(Component::Prefix(p)),
            Component::RootDir => components.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => match components.last() {
                Some(Component::Normal(_)) => {
                    components.pop();
                }
                Some(Component::RootDir) => {}
                _ => {
                    components.push(Component::ParentDir);
                }
            },
            Component::Normal(c) => components.push(Component::Normal(c)),
        }
    }

    components.into_iter().collect()
}

pub fn resolve_workspace_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let canonical_root = match dunce::canonicalize(root) {
        Ok(p) => p,
        Err(_) => {
            let abs = if root.is_absolute() {
                root.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(root)
            };
            normalize_path(&abs)
        }
    };

    let rel_path = Path::new(relative);
    let candidate = if rel_path.is_absolute() {
        rel_path.to_path_buf()
    } else {
        canonical_root.join(rel_path)
    };

    if candidate.exists() {
        let canonical_candidate = dunce::canonicalize(&candidate)
            .map_err(|e| format!("Failed to canonicalize path '{}': {e}", candidate.display()))?;

        if !canonical_candidate.starts_with(&canonical_root) {
            return Err("Path escapes workspace boundary".to_string());
        }

        Ok(canonical_candidate)
    } else {
        let normalized = normalize_path(&candidate);

        if !normalized.starts_with(&canonical_root) {
            return Err("Path escapes workspace boundary".to_string());
        }

        let mut ancestor = normalized.as_path();
        let mut non_existing_components = Vec::new();

        while !ancestor.exists() {
            if let Some(parent) = ancestor.parent() {
                if let Some(file_name) = ancestor.file_name() {
                    non_existing_components.push(file_name);
                }
                ancestor = parent;
            } else {
                break;
            }
        }

        let canonical_ancestor = if ancestor.exists() {
            dunce::canonicalize(ancestor).map_err(|e| {
                format!(
                    "Failed to canonicalize ancestor '{}': {e}",
                    ancestor.display()
                )
            })?
        } else {
            ancestor.to_path_buf()
        };

        if !canonical_ancestor.starts_with(&canonical_root) {
            return Err("Path escapes workspace boundary".to_string());
        }

        let mut canonical_path = canonical_ancestor;
        for comp in non_existing_components.into_iter().rev() {
            canonical_path.push(comp);
        }

        if !canonical_path.starts_with(&canonical_root) {
            return Err("Path escapes workspace boundary".to_string());
        }

        Ok(canonical_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_file_read_write_edit_cycle() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        let write_tool = FileWriteTool;
        let read_tool = FileReadTool;
        let edit_tool = FileEditTool;

        // 1. Write file
        let write_res = write_tool
            .execute(
                json!({
                    "file_path": "sub/hello.txt",
                    "content": "Line 1\nHello World\nLine 3"
                }),
                root,
            )
            .await;
        assert!(write_res.is_ok());

        // 2. Read file with offset and limit
        let read_res = read_tool
            .execute(
                json!({
                    "file_path": "sub/hello.txt",
                    "offset": 1,
                    "limit": 1
                }),
                root,
            )
            .await
            .unwrap();
        assert_eq!(read_res, "Hello World");

        // 3. Edit file
        let edit_res = edit_tool
            .execute(
                json!({
                    "file_path": "sub/hello.txt",
                    "target": "Hello World",
                    "replacement": "Greetings Universe"
                }),
                root,
            )
            .await;
        assert!(edit_res.is_ok());

        // 4. Verify edited content
        let full_read = read_tool
            .execute(json!({ "file_path": "sub/hello.txt" }), root)
            .await
            .unwrap();
        assert_eq!(full_read, "Line 1\nGreetings Universe\nLine 3");
    }

    #[tokio::test]
    async fn test_glob_tool() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::write(root.join("src/main.rs"), "fn main() {}")
            .await
            .unwrap();
        tokio::fs::write(root.join("src/lib.rs"), "pub fn lib() {}")
            .await
            .unwrap();
        tokio::fs::write(root.join("Cargo.toml"), "[package]")
            .await
            .unwrap();

        let glob_tool = GlobTool;
        let res = glob_tool
            .execute(json!({ "pattern": "**/*.rs" }), root)
            .await
            .unwrap();

        assert!(res.contains("src/main.rs") || res.contains("src\\main.rs"));
        assert!(res.contains("src/lib.rs") || res.contains("src\\lib.rs"));
        assert!(!res.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_grep_tool() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::write(root.join("src/test.txt"), "apple\nBanana\nCherry")
            .await
            .unwrap();

        let grep_tool = GrepTool;
        let res_case = grep_tool
            .execute(
                json!({
                    "pattern": "banana",
                    "case_sensitive": false
                }),
                root,
            )
            .await
            .unwrap();

        assert!(res_case.contains("Banana"));
        assert!(res_case.contains("2: Banana"));
    }

    #[tokio::test]
    async fn test_shell_tool_execution() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        let shell_tool = ShellTool;
        let res = shell_tool
            .execute(
                json!({
                    "command": "Write-Output 'JobObjectVerified'",
                    "timeout_ms": 10000
                }),
                root,
            )
            .await
            .unwrap();

        assert!(res.contains("JobObjectVerified"));
    }

    #[tokio::test]
    async fn test_workspace_boundary_confinement() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // 1. Valid relative paths inside workspace
        let inside = resolve_workspace_path(root, "valid/sub/file.txt");
        assert!(inside.is_ok());

        // 2. Traversal outside workspace via ..
        let escape_relative = resolve_workspace_path(root, "../../escaped.txt");
        assert_eq!(
            escape_relative.unwrap_err(),
            "Path escapes workspace boundary"
        );

        let escape_nested = resolve_workspace_path(root, "sub/../../../../escaped.txt");
        assert_eq!(
            escape_nested.unwrap_err(),
            "Path escapes workspace boundary"
        );

        // 3. Absolute path outside workspace
        #[cfg(target_os = "windows")]
        let outside_abs = "C:\\Windows\\System32\\drivers\\etc\\hosts";
        #[cfg(not(target_os = "windows"))]
        let outside_abs = "/etc/passwd";

        let escape_abs = resolve_workspace_path(root, outside_abs);
        assert_eq!(escape_abs.unwrap_err(), "Path escapes workspace boundary");

        // 4. Absolute path inside workspace
        let canonical_root = dunce::canonicalize(root).unwrap();
        let inside_abs = canonical_root.join("sub").join("nested.txt");
        let res_inside_abs = resolve_workspace_path(root, inside_abs.to_str().unwrap());
        assert!(res_inside_abs.is_ok());

        // 5. FileReadTool blocking path escape
        let read_tool = FileReadTool;
        let read_err = read_tool
            .execute(json!({ "file_path": "../../secret.txt" }), root)
            .await;
        assert_eq!(read_err.unwrap_err(), "Path escapes workspace boundary");

        // 6. FileWriteTool blocking path escape
        let write_tool = FileWriteTool;
        let write_err = write_tool
            .execute(
                json!({
                    "file_path": "../evil.txt",
                    "content": "hacked"
                }),
                root,
            )
            .await;
        assert_eq!(write_err.unwrap_err(), "Path escapes workspace boundary");
    }
}
