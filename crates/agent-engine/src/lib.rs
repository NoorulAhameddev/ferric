pub mod autofix;
pub mod checkpoint;
pub mod compactor;
pub mod config;
pub mod context;
pub mod loop_engine;
pub mod mcp;
pub mod permissions;
pub mod session;
pub mod subagents;
pub mod tasks;
pub mod tools;

pub use autofix::{
    detect_test_runner, format_repair_prompt, parse_custom_test_cmd, parse_test_failures,
    run_test_suite, AutoFixReport, TestFailure, TestResult, TestRunner,
};
pub use checkpoint::{Checkpoint, CheckpointManager, RollbackResult};
pub use compactor::{
    compact_conversation, microcompact_messages, MicrocompactResult, DEFAULT_KEEP_RECENT_TURNS,
    OLD_TOOL_RESULT_CLEARED_MESSAGE,
};
pub use config::{AgentConfig, McpServerEntry, ProviderConfig};
pub use context::{
    assemble_system_prompt, build_system_prompt, detect_git_context, discover_claude_md,
    discover_project_instructions, GitContext, DEFAULT_BASE_PERSONA,
};

pub use loop_engine::{AgentLoop, PermissionHandler};
pub use permissions::{PermissionDecision, PermissionMode, PermissionPolicy};
pub use session::{SessionManager, TranscriptEntry};
pub use subagents::{
    AgentTool, SendMessageTool, SubagentId, SubagentInfo, SubagentManager, SubagentStatus,
};
pub use tasks::{
    TaskContext, TaskCreateTool, TaskGetTool, TaskId, TaskInfo, TaskListTool, TaskManager,
    TaskStatus, TaskStopTool,
};
pub use tools::{
    AskUserQuestionTool, EnterPlanModeTool, ExitPlanModeTool, FileEditTool, FileReadTool,
    FileWriteTool, GlobTool, GrepTool, NotebookEditTool, PlanModeTool, ShellTool, SkillTool,
    TodoWriteTool, Tool, WebFetchTool, WebSearchTool,
};
