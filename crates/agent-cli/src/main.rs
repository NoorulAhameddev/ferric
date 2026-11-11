use agent_core::LlmProvider;
use agent_engine::{
    AgentConfig, AgentLoop, AgentTool, McpServerEntry, PermissionMode, SendMessageTool,
    SubagentManager, TaskCreateTool, TaskGetTool, TaskListTool, TaskManager, TaskStopTool,
};
use agent_providers::{
    AnthropicProvider, GeminiProvider, GenericOpenAiCompatibleProvider, OpenAiProvider,
};
use clap::Parser;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "agent",
    version = "0.1.0",
    about = "Universal High-Performance Rust AI Coding Agent",
    after_help = "Secondary Tools:
  NotebookEdit     Edit Jupyter notebook (.ipynb) cells
  WebFetch         Fetch and extract content from a web URL
  WebSearch        Execute web search queries
  TodoWrite        Track and update task list items
  Skill            Invoke custom skills and specialized workflows
  PlanMode         Toggle structured planning mode
  AskUserQuestion  Prompt the user with interactive questions"
)]
pub struct Cli {
    #[arg(short, long, help = "Print response and exit (non-interactive mode)")]
    pub print: bool,

    #[arg(short, long, help = "Model ID to use (overrides config)")]
    pub model: Option<String>,

    #[arg(
        long,
        help = "LLM provider: anthropic, openai, gemini, ollama, openrouter (overrides config)"
    )]
    pub provider: Option<String>,

    #[arg(long, help = "Base URL for custom OpenAI-compatible endpoints")]
    pub base_url: Option<String>,

    #[arg(
        long,
        help = "Permission mode: default, acceptEdits, bypassPermissions, plan"
    )]
    pub permission_mode: Option<String>,

    #[arg(long, help = "Resume a previous session by full ID or prefix")]
    pub resume: Option<String>,

    #[arg(
        short = 'c',
        long = "continue",
        help = "Continue the most recent session"
    )]
    pub r#continue: bool,

    #[arg(long = "list-sessions", help = "List all saved sessions and exit")]
    pub list_sessions: bool,

    #[arg(
        long,
        help = "Automatically verify and repair failing tests after turns"
    )]
    pub auto_fix: bool,

    #[arg(
        long,
        help = "Explicit test command for auto-fix (e.g. 'cargo test', 'npm test')"
    )]
    pub test_cmd: Option<String>,

    #[arg(
        long,
        default_value = "3",
        help = "Maximum repair retries in auto-fix mode"
    )]
    pub max_fix_retries: usize,

    #[arg(help = "User prompt. If omitted, reads from stdin")]
    pub prompt: Option<String>,
}

/// Registers the subagent and task management tools into an AgentLoop.
pub fn register_subagent_and_task_tools(agent: &mut AgentLoop) {
    let task_manager = TaskManager::new();
    let subagent_manager = SubagentManager::new();

    agent.register_tool(Box::new(TaskCreateTool::new(task_manager.clone())));
    agent.register_tool(Box::new(TaskListTool::new(task_manager.clone())));
    agent.register_tool(Box::new(TaskGetTool::new(task_manager.clone())));
    agent.register_tool(Box::new(TaskStopTool::new(task_manager.clone())));

    let send_message = SendMessageTool::new()
        .with_task_manager(task_manager.clone())
        .with_subagent_manager(subagent_manager.clone());
    agent.register_tool(Box::new(send_message));

    let agent_tool = AgentTool::from_agent_loop(agent)
        .with_task_manager(task_manager)
        .with_subagent_manager(subagent_manager);
    agent.register_tool(Box::new(agent_tool));
}

/// Connects, initializes, and registers tools from configured MCP servers.
pub async fn register_mcp_servers(
    agent: &mut AgentLoop,
    mcp_servers: &HashMap<String, McpServerEntry>,
    cwd: &Path,
) {
    for (server_name, entry) in mcp_servers {
        let transport_res: Result<Arc<dyn agent_engine::mcp::McpTransport>, _> = match entry {
            McpServerEntry::Stdio { command, args, env } => {
                agent_engine::mcp::StdioTransport::spawn(command, args, env, cwd)
                    .map(|t| Arc::new(t) as Arc<dyn agent_engine::mcp::McpTransport>)
                    .map_err(|e| e.to_string())
            }
            McpServerEntry::Http { url, headers } => {
                agent_engine::mcp::StreamableHttpTransport::new(url, headers.clone(), true)
                    .map(|t| Arc::new(t) as Arc<dyn agent_engine::mcp::McpTransport>)
                    .map_err(|e| e.to_string())
            }
        };

        match transport_res {
            Ok(transport) => {
                let mut client = agent_engine::mcp::McpClient::new(transport);
                if let Err(e) = client.initialize().await {
                    eprintln!("Warning: failed to initialize MCP server '{server_name}': {e}");
                    continue;
                }
                match client.list_tools().await {
                    Ok(tools) => {
                        let client_arc = Arc::new(tokio::sync::Mutex::new(client));
                        for tool in tools {
                            let desc = tool.description.unwrap_or_default();
                            let wrapper = agent_engine::mcp::McpToolWrapper::new(
                                server_name.clone(),
                                tool.name,
                                desc,
                                tool.input_schema,
                                client_arc.clone(),
                            );
                            agent.register_tool(Box::new(wrapper));
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to list tools for MCP server '{server_name}': {e}"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: failed to start transport for MCP server '{server_name}': {e}");
            }
        }
    }
}

/// Extracts tool calls and their matched outputs from messages generated during a turn.
pub fn extract_turn_tool_calls(
    messages: &[agent_core::Message],
    start_idx: usize,
) -> Vec<agent_tui::UiToolCall> {
    let mut tool_results: HashMap<String, (String, bool)> = HashMap::new();

    let msgs = if start_idx < messages.len() {
        &messages[start_idx..]
    } else {
        &[]
    };

    for msg in msgs {
        for block in &msg.content {
            if let agent_core::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                tool_results.insert(tool_use_id.clone(), (content.clone(), *is_error));
            }
        }
    }

    let mut tool_calls = Vec::new();
    for msg in msgs {
        for block in &msg.content {
            if let agent_core::ContentBlock::ToolUse { id, name, input } = block {
                let (output, is_error) = tool_results
                    .get(id)
                    .map(|(out, err)| (Some(out.clone()), *err))
                    .unwrap_or((None, false));
                tool_calls.push(agent_tui::UiToolCall {
                    name: name.clone(),
                    input: input.to_string(),
                    output,
                    is_error,
                });
            }
        }
    }

    tool_calls
}

/// Converts historical agent_core::Message records into agent_tui::UiMessage structures for display.
pub fn convert_messages_to_ui(messages: &[agent_core::Message]) -> Vec<agent_tui::UiMessage> {
    let mut ui_msgs = Vec::new();
    let mut tool_results: HashMap<String, (String, bool)> = HashMap::new();

    for msg in messages {
        for block in &msg.content {
            if let agent_core::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                tool_results.insert(tool_use_id.clone(), (content.clone(), *is_error));
            }
        }
    }

    for msg in messages {
        match msg.role {
            agent_core::Role::User => {
                let mut text = String::new();
                for block in &msg.content {
                    if let agent_core::ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    ui_msgs.push(agent_tui::UiMessage {
                        role: agent_core::Role::User,
                        text,
                        thinking: None,
                        tool_calls: Vec::new(),
                    });
                }
            }
            agent_core::Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for block in &msg.content {
                    match block {
                        agent_core::ContentBlock::Text { text: t } => text.push_str(t),
                        agent_core::ContentBlock::ToolUse { id, name, input } => {
                            let (out, is_err) = tool_results
                                .get(id)
                                .map(|(o, e)| (Some(o.clone()), *e))
                                .unwrap_or((None, false));
                            tool_calls.push(agent_tui::UiToolCall {
                                name: name.clone(),
                                input: input.to_string(),
                                output: out,
                                is_error: is_err,
                            });
                        }
                        _ => {}
                    }
                }
                ui_msgs.push(agent_tui::UiMessage {
                    role: agent_core::Role::Assistant,
                    text,
                    thinking: None,
                    tool_calls,
                });
            }
            _ => {}
        }
    }
    ui_msgs
}

/// Resolves standard persistent prompt history file path (~/.agent_history)
fn get_history_file_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile).join(".agent_history");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".agent_history");
    }
    PathBuf::from(".agent_history")
}

/// Ensures local free gateway services (OpenCode and Node proxy) are listening if connecting to port 4097.
async fn ensure_local_gateway_if_needed(base_url: Option<&str>) {
    let Some(url) = base_url else {
        return;
    };
    if !url.contains(":4097") {
        return;
    }

    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr = SocketAddr::from(([127, 0, 0, 1], 4097));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
        return;
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let script = r#"
            $op = if ($env:OPENCODE_EXE -and (Test-Path $env:OPENCODE_EXE)) { $env:OPENCODE_EXE } else {
                $cmd = Get-Command opencode.exe -ErrorAction SilentlyContinue
                if ($cmd) { $cmd.Source } else {
                    Get-ChildItem -Path "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Filter "opencode.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName -First 1
                }
            }
            $px = if ($env:FREE_AI_GATEWAY_PROXY -and (Test-Path $env:FREE_AI_GATEWAY_PROXY)) { $env:FREE_AI_GATEWAY_PROXY } else {
                @(
                    "$PSScriptRoot\..\free-ai-gateway\proxy.mjs",
                    "$PSScriptRoot\free-ai-gateway\proxy.mjs",
                    "$env:USERPROFILE\free-ai-gateway\proxy.mjs",
                    "$env:USERPROFILE\Downloads\REA\free_ai_gateway\proxy.mjs"
                ) | Where-Object { Test-Path $_ } | Select-Object -First 1
            }
            if ($op -and (Test-Path $op)) {
                Start-Process -FilePath $op -ArgumentList "serve --port 4096 --auto" -WindowStyle Hidden
            }
            if ($px -and (Test-Path $px)) {
                Start-Process -FilePath "node" -ArgumentList "`"$px`"" -WindowStyle Hidden
            }
        "#;
        let _ = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .creation_flags(0x08000000)
            .spawn();

        for _ in 0..15 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                break;
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Cli::parse();

    // 1. Resolve workspace root and configuration hierarchy
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut config = AgentConfig::load(&cwd);

    // Apply CLI overrides over configuration
    if let Some(p) = args.provider {
        config.default_provider = p;
    }
    if let Some(m) = args.model {
        config.default_model = m;
    }
    if let Some(ref pm) = args.permission_mode {
        config.permissions.default_mode = match pm.to_lowercase().as_str() {
            "bypasstermissions" => PermissionMode::BypassPermissions,
            "acceptedits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            _ => PermissionMode::Default,
        };
    }

    // Auto-heal local gateway if configured
    let base_url_owned = config.get_base_url(&config.default_provider);
    let resolved_base_url = args.base_url.as_deref().or(base_url_owned.as_deref());
    ensure_local_gateway_if_needed(resolved_base_url).await;

    // 2. Instantiate selected provider with resolved credentials
    let provider_name = config.default_provider.to_lowercase();
    let provider: Arc<dyn LlmProvider> = match provider_name.as_str() {
        "anthropic" => {
            let key = config.get_api_key("anthropic");
            let mut p = AnthropicProvider::new(key);
            if let Some(url) = args
                .base_url
                .clone()
                .or_else(|| config.get_base_url("anthropic"))
            {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openai" => {
            let key = config.get_api_key("openai");
            let mut p = OpenAiProvider::new(key);
            if let Some(url) = args
                .base_url
                .clone()
                .or_else(|| config.get_base_url("openai"))
            {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "gemini" => {
            let key = config.get_api_key("gemini");
            Arc::new(GeminiProvider::new(key))
        }
        "ollama" => {
            let url = args
                .base_url
                .clone()
                .or_else(|| config.get_base_url("ollama"));
            Arc::new(GenericOpenAiCompatibleProvider::ollama(url.as_deref()))
        }
        "openrouter" => {
            let key = config.get_api_key("openrouter");
            Arc::new(GenericOpenAiCompatibleProvider::openrouter(key))
        }
        _ => {
            let key = config.get_api_key(&provider_name);
            let url = args
                .base_url
                .clone()
                .or_else(|| config.get_base_url(&provider_name))
                .unwrap_or_else(|| "http://localhost:8000/v1".into());
            Arc::new(GenericOpenAiCompatibleProvider::new(
                provider_name.clone(),
                provider_name,
                url,
                key,
            ))
        }
    };

    // 3. Initialize workspace and session paths
    let sessions_dir = cwd.join(".agent_sessions");

    if args.list_sessions {
        let sessions = agent_engine::SessionManager::list_sessions(&sessions_dir)
            .map_err(|e| anyhow::anyhow!(e))?;
        if sessions.is_empty() {
            println!("No saved sessions found in {}", sessions_dir.display());
        } else {
            println!(
                "{:<38} {:<22} {:<8} {:<24}",
                "SESSION ID", "UPDATED AT", "TURNS", "TITLE"
            );
            println!("{}", "-".repeat(95));
            for s in sessions {
                let dt = s.updated_at.format("%Y-%m-%d %H:%M:%S").to_string();
                println!(
                    "{:<38} {:<22} {:<8} {:<24}",
                    s.session_id, dt, s.turn_count, s.title
                );
            }
        }
        return Ok(());
    }

    let resume_session = if let Some(ref target) = args.resume {
        let meta = agent_engine::SessionManager::find_session(&sessions_dir, target)
            .map_err(|e| anyhow::anyhow!(e))?;
        let msgs = agent_engine::SessionManager::load_transcript(&sessions_dir, &meta.session_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        Some((meta, msgs))
    } else if args.r#continue {
        let meta = agent_engine::SessionManager::latest_session(&sessions_dir)
            .map_err(|e| anyhow::anyhow!(e))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No existing sessions found in {} to continue",
                    sessions_dir.display()
                )
            })?;
        let msgs = agent_engine::SessionManager::load_transcript(&sessions_dir, &meta.session_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        Some((meta, msgs))
    } else {
        None
    };

    let (mut agent, historical_ui_messages) = if let Some((meta, resumed_msgs)) = resume_session {
        eprintln!(
            "Resuming session {} ({} historical messages)...",
            meta.session_id,
            resumed_msgs.len()
        );
        let ui_msgs = convert_messages_to_ui(&resumed_msgs);
        let agent = AgentLoop::resume(
            provider,
            cwd.clone(),
            config.permissions,
            sessions_dir,
            meta,
            resumed_msgs,
        );
        (agent, ui_msgs)
    } else {
        let agent = AgentLoop::new(
            provider,
            cwd.clone(),
            config.permissions,
            sessions_dir,
            config.default_model.clone(),
        );
        (agent, Vec::new())
    };

    // 4. Register Subagent tools and Task tools
    register_subagent_and_task_tools(&mut agent);

    // 5. Register configured MCP servers
    register_mcp_servers(&mut agent, &config.mcp_servers, &cwd).await;

    // 6. Resolve prompt from CLI arg or stdin pipe
    let prompt = if let Some(p) = args.prompt {
        p
    } else {
        let mut buffer = String::new();
        let mut stdin = std::io::stdin();
        if !std::io::IsTerminal::is_terminal(&stdin) {
            stdin.read_to_string(&mut buffer)?;
        }
        buffer.trim().to_string()
    };

    if prompt.is_empty() {
        if !args.print && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            let mut app = agent_tui::AppState::new(
                config.default_model.clone(),
                config.default_provider.clone(),
            )
            .with_history_file(get_history_file_path());
            app.messages = historical_ui_messages;

            // Wire the interactive TUI permission handler channel
            let (update_tx, update_rx) = tokio::sync::mpsc::unbounded_channel();
            let perm_handler = Arc::new(agent_tui::TuiPermissionHandler::new(update_tx.clone()));
            agent.set_permission_handler(perm_handler);

            // Bridge agent stream events (thinking, tokens, tool status) to TUI ExecutionUpdate!
            let (event_tx, mut event_rx) =
                tokio::sync::mpsc::unbounded_channel::<agent_core::StreamEvent>();
            agent.set_event_sender(event_tx);

            let fwd_tx = update_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = event_rx.recv().await {
                    match ev {
                        agent_core::StreamEvent::ThinkingDelta { text } => {
                            let _ = fwd_tx.send(agent_tui::ExecutionUpdate::Thinking(text));
                        }
                        agent_core::StreamEvent::TextDelta { text } => {
                            let _ = fwd_tx.send(agent_tui::ExecutionUpdate::Token(text));
                        }
                        agent_core::StreamEvent::StatusUpdate { status } => {
                            let _ = fwd_tx.send(agent_tui::ExecutionUpdate::Activity(status));
                        }
                        _ => {}
                    }
                }
            });

            let agent = Arc::new(tokio::sync::Mutex::new(agent));
            let runner = {
                let agent = Arc::clone(&agent);
                move |prompt: String| {
                    let agent = Arc::clone(&agent);
                    async move {
                        let mut guard = agent.lock().await;
                        if prompt.trim() == "/undo" {
                            return match guard.undo() {
                                Ok(rollback) => {
                                    let restored = if rollback.restored_files.is_empty() {
                                        "none".to_string()
                                    } else {
                                        rollback.restored_files.join(", ")
                                    };
                                    let deleted = if rollback.deleted_files.is_empty() {
                                        "none".to_string()
                                    } else {
                                        rollback.deleted_files.join(", ")
                                    };
                                    let turn = rollback.turn;
                                    let text = format!(
                                        "Rolled back turn {turn}.\n  • Restored file(s): {restored}\n  • Removed untracked file(s): {deleted}"
                                    );
                                    Ok(agent_tui::TurnResult::new(text))
                                }
                                Err(e) => {
                                    Ok(agent_tui::TurnResult::new(format!("Undo failed: {e}")))
                                }
                            };
                        }

                        let msg_count_before = guard.messages().len();
                        let text = guard.run(&prompt).await?;
                        let tool_calls =
                            extract_turn_tool_calls(guard.messages(), msg_count_before);
                        let usage = guard.total_usage().clone();
                        let thinking = guard.last_turn_thinking().map(String::from);
                        Ok(agent_tui::TurnResult::new(text)
                            .with_thinking(thinking)
                            .with_tool_calls(tool_calls)
                            .with_usage(usage))
                    }
                }
            };

            if let Err(e) =
                agent_tui::run_tui_with_runner_and_updates(&mut app, runner, update_rx).await
            {
                eprintln!("TUI session terminated: {e}");
            }
            return Ok(());
        }

        println!("Universal Rust AI Coding Agent v0.1.0");
        println!(
            "Provider: {} | Model: {}",
            config.default_provider, config.default_model
        );
        println!("Usage: agent [OPTIONS] [PROMPT]");
        println!("Run 'agent --help' for available flags.");
        return Ok(());
    }

    // 7. Execute turn in non-interactive/print CLI mode
    if args.auto_fix {
        eprintln!("🚀 Autonomous test verification and auto-repair active...");
        let report = agent
            .run_with_autofix(&prompt, args.test_cmd.as_deref(), args.max_fix_retries)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if args.print {
            println!(
                "{}",
                report.history.last().unwrap_or(&report.initial_response)
            );
        } else {
            println!("\n=== Autonomous Auto-Fix Report ===");
            println!("Verification attempts: {}", report.attempts);
            println!(
                "Final test suite status: {}",
                if report.final_tests_passed {
                    "PASSED ✔"
                } else {
                    "FAILED ✖"
                }
            );
            println!(
                "\n=== Final Response ===\n{}",
                report.history.last().unwrap_or(&report.initial_response)
            );
        }
        return Ok(());
    }

    let result = agent.run(&prompt).await.map_err(|e| anyhow::anyhow!(e))?;

    if args.print {
        println!("{result}");
    } else {
        println!("\n=== Response ===\n{result}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ContentBlock, Message, Role};

    #[test]
    fn test_cli_parsing_flags() {
        let args = Cli::parse_from([
            "agent",
            "--print",
            "--model",
            "ferric-pro",
            "--provider",
            "anthropic",
            "--permission-mode",
            "bypasstermissions",
            "fix the tests",
        ]);

        assert!(args.print);
        assert_eq!(args.model.as_deref(), Some("ferric-pro"));
        assert_eq!(args.provider.as_deref(), Some("anthropic"));
        assert_eq!(args.permission_mode.as_deref(), Some("bypasstermissions"));
        assert_eq!(args.prompt.as_deref(), Some("fix the tests"));
    }

    #[test]
    fn test_extract_turn_tool_calls_matching() {
        let messages = vec![
            Message::user_text("Run grep on src"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Searching for function...".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_123".into(),
                        name: "grep".into(),
                        input: serde_json::json!({ "pattern": "fn main" }),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_123".into(),
                    content: "src/main.rs:1:fn main() {}".into(),
                    is_error: false,
                }],
            },
        ];

        let extracted = extract_turn_tool_calls(&messages, 1);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].name, "grep");
        assert!(extracted[0].input.contains("fn main"));
        assert_eq!(
            extracted[0].output.as_deref(),
            Some("src/main.rs:1:fn main() {}")
        );
        assert!(!extracted[0].is_error);
    }

    #[test]
    fn test_extract_turn_tool_calls_error() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_err".into(),
                    name: "shell".into(),
                    input: serde_json::json!({ "command": "false" }),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_err".into(),
                    content: "Command exited with code 1".into(),
                    is_error: true,
                }],
            },
        ];

        let extracted = extract_turn_tool_calls(&messages, 0);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].name, "shell");
        assert!(extracted[0].is_error);
        assert_eq!(
            extracted[0].output.as_deref(),
            Some("Command exited with code 1")
        );
    }

    #[test]
    fn test_cli_help_documents_secondary_tools() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        for tool in [
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Skill",
            "PlanMode",
            "AskUserQuestion",
        ] {
            assert!(
                help.contains(tool),
                "CLI help should document secondary tool: {tool}"
            );
        }
    }

    #[test]
    fn test_agent_loop_secondary_tools_registered() {
        use agent_engine::AgentLoop;
        use agent_providers::AnthropicProvider;
        use std::sync::Arc;

        let provider = Arc::new(AnthropicProvider::new("test_key"));
        let agent = AgentLoop::new(
            provider,
            std::env::current_dir().unwrap(),
            agent_engine::PermissionPolicy::default(),
            std::env::temp_dir(),
            "ferric-pro",
        );

        let tools = agent.tools();
        for expected in [
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Skill",
            "AskUserQuestion",
        ] {
            assert!(
                tools.contains_key(expected),
                "AgentLoop should have secondary tool '{expected}' registered"
            );
        }
        assert!(
            tools.contains_key("EnterPlanMode") && tools.contains_key("ExitPlanMode"),
            "AgentLoop should have PlanMode tools registered"
        );
    }
}
