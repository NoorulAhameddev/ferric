use crate::checkpoint::{Checkpoint, CheckpointManager, RollbackResult};
use crate::permissions::{PermissionDecision, PermissionPolicy};
use crate::session::{SessionManager, TranscriptEntry};
use crate::tools::{
    AskUserQuestionTool, EnterPlanModeTool, ExitPlanModeTool, FileEditTool, FileReadTool,
    FileWriteTool, GitCommitTool, GitDiffTool, GitStatusTool, GlobTool, GrepTool, NotebookEditTool,
    PatchApplyTool, ShellTool, SkillTool, TodoWriteTool, Tool, WebFetchTool, WebSearchTool,
};
use agent_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, Role, ToolCallAccumulator,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[async_trait]
pub trait PermissionHandler: Send + Sync {
    async fn request_permission(&self, tool_name: &str, input: &serde_json::Value) -> bool;
}

#[async_trait]
impl<F> PermissionHandler for F
where
    F: Fn(&str, &serde_json::Value) -> bool + Send + Sync,
{
    async fn request_permission(&self, tool_name: &str, input: &serde_json::Value) -> bool {
        self(tool_name, input)
    }
}

pub struct AgentLoop {
    provider: Arc<dyn LlmProvider>,
    tools: HashMap<String, Box<dyn Tool>>,
    policy: PermissionPolicy,
    session: SessionManager,
    workspace_root: PathBuf,
    system_prompt: Option<String>,
    messages: Vec<Message>,
    max_turns: u32,
    permission_handler: Option<Arc<dyn PermissionHandler>>,
    event_sender: Option<tokio::sync::mpsc::UnboundedSender<agent_core::StreamEvent>>,
    last_turn_thinking: Option<String>,
    checkpoint_mgr: CheckpointManager,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        workspace_root: impl Into<PathBuf>,
        policy: PermissionPolicy,
        sessions_dir: impl Into<PathBuf>,
        model: impl Into<String>,
    ) -> Self {
        let model_str = model.into();
        let session = SessionManager::new(sessions_dir, &model_str);
        let workspace_root = workspace_root.into();
        let session_id = session.metadata().session_id.clone();
        let checkpoint_mgr = CheckpointManager::new(&workspace_root, session_id);
        let system_prompt = crate::context::build_system_prompt(&workspace_root, None);
        let mut loop_engine = Self {
            provider,
            tools: HashMap::new(),
            policy,
            session,
            workspace_root,
            system_prompt: Some(system_prompt),
            messages: Vec::new(),
            max_turns: 25,
            permission_handler: None,
            event_sender: None,
            last_turn_thinking: None,
            checkpoint_mgr,
        };

        loop_engine.register_builtin_tools();
        loop_engine
    }

    pub fn resume(
        provider: Arc<dyn LlmProvider>,
        workspace_root: impl Into<PathBuf>,
        policy: PermissionPolicy,
        sessions_dir: impl Into<PathBuf>,
        metadata: crate::session::SessionMetadata,
        messages: Vec<Message>,
    ) -> Self {
        let workspace_root = workspace_root.into();
        let sessions_dir = sessions_dir.into();
        let session_id = metadata.session_id.clone();
        let checkpoint_mgr = CheckpointManager::new(&workspace_root, session_id);
        let system_prompt = crate::context::build_system_prompt(&workspace_root, None);
        let session = SessionManager::from_existing(sessions_dir, metadata);

        let mut loop_engine = Self {
            provider,
            tools: HashMap::new(),
            policy,
            session,
            workspace_root,
            system_prompt: Some(system_prompt),
            messages,
            max_turns: 25,
            permission_handler: None,
            event_sender: None,
            last_turn_thinking: None,
            checkpoint_mgr,
        };

        loop_engine.register_builtin_tools();
        loop_engine
    }

    fn register_builtin_tools(&mut self) {
        self.register_tool(Box::new(FileReadTool));
        self.register_tool(Box::new(FileWriteTool));
        self.register_tool(Box::new(FileEditTool));
        self.register_tool(Box::new(GlobTool));
        self.register_tool(Box::new(GrepTool));
        self.register_tool(Box::new(ShellTool));
        self.register_tool(Box::new(NotebookEditTool::new()));
        self.register_tool(Box::new(WebFetchTool::new()));
        self.register_tool(Box::new(WebSearchTool::new()));
        self.register_tool(Box::new(TodoWriteTool::new()));
        self.register_tool(Box::new(SkillTool::new()));
        self.register_tool(Box::new(EnterPlanModeTool::new()));
        self.register_tool(Box::new(ExitPlanModeTool::new()));
        self.register_tool(Box::new(AskUserQuestionTool::new()));
        self.register_tool(Box::new(GitStatusTool::new()));
        self.register_tool(Box::new(GitDiffTool::new()));
        self.register_tool(Box::new(GitCommitTool::new()));
        self.register_tool(Box::new(PatchApplyTool::new()));
    }

    /// Reverts files touched during the most recent turn using Git checkpointing
    pub fn undo(&mut self) -> Result<RollbackResult, String> {
        self.checkpoint_mgr.rollback_latest()
    }

    pub fn checkpoints(&self) -> &[Checkpoint] {
        self.checkpoint_mgr.checkpoints()
    }

    pub fn tools(&self) -> &HashMap<String, Box<dyn Tool>> {
        &self.tools
    }

    pub fn set_event_sender(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<agent_core::StreamEvent>,
    ) {
        self.event_sender = Some(tx);
    }

    pub fn last_turn_thinking(&self) -> Option<&str> {
        self.last_turn_thinking.as_deref()
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.definition().name, tool);
    }

    /// Executes an autonomous agent turn sequence until tool calling concludes or max turns reached.
    pub async fn run(&mut self, user_input: &str) -> Result<String, String> {
        let checkpoint_turn = self.session.metadata().turn_count + 1;
        if let Err(e) = self.checkpoint_mgr.create_checkpoint(checkpoint_turn) {
            tracing::warn!("Failed to capture git checkpoint: {e}");
        }

        self.last_turn_thinking = None;
        let user_msg = Message::user_text(user_input);
        self.messages.push(user_msg.clone());
        self.session
            .record_entry(&TranscriptEntry::Message { message: user_msg })
            .await?;

        let mut turn = 0;
        let mut final_text = String::new();

        while turn < self.max_turns {
            turn += 1;

            // Microcompact earlier tool results to conserve context window
            crate::compactor::microcompact_messages(
                &mut self.messages,
                crate::compactor::DEFAULT_KEEP_RECENT_TURNS,
            );

            // Summarize earlier conversation turns if message history exceeds 30
            crate::compactor::compact_conversation(&mut self.messages, 30, 6);

            let tool_defs = self.tools.values().map(|t| t.definition()).collect();

            let req = CompletionRequest {
                model: self.session.metadata().model.clone(),
                messages: self.messages.clone(),
                tools: tool_defs,
                system_prompt: self.system_prompt.clone(),
                generation: Default::default(),
                reasoning: None,
                cache_policy: None,
                extensions: Default::default(),
            };

            let mut stream = self.provider.stream(req).await.map_err(|e| e.to_string())?;
            let mut accumulator = ToolCallAccumulator::default();

            while let Some(event_res) = stream.next().await {
                let event = event_res.map_err(|e| e.to_string())?;
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(event.clone());
                }
                accumulator
                    .process_event(&event)
                    .map_err(|e| e.to_string())?;
            }

            if let Some(think) = accumulator.thinking() {
                self.last_turn_thinking = Some(think.to_string());
            }

            let (assistant_msg, usage) = accumulator.finalize().map_err(|e| e.to_string())?;
            self.session.record_turn_usage(usage.as_ref()).await?;
            self.session
                .record_entry(&TranscriptEntry::Message {
                    message: assistant_msg.clone(),
                })
                .await?;

            // Extract text responses
            for block in &assistant_msg.content {
                if let ContentBlock::Text { text } = block {
                    final_text.push_str(text);
                }
            }

            let mut tool_results = Vec::new();

            // Process tool use requests
            for block in &assistant_msg.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let target = input.to_string();
                    let decision = self.policy.evaluate(name, &target);

                    let target_desc =
                        if let Some(path) = input.get("file_path").and_then(|p| p.as_str()) {
                            format!("Reading/Editing {path}")
                        } else if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                            format!("Running `{cmd}`")
                        } else {
                            format!("Executing {name}")
                        };

                    if let Some(ref tx) = self.event_sender {
                        let _ = tx.send(agent_core::StreamEvent::StatusUpdate {
                            status: target_desc.clone(),
                        });
                    }

                    let (output, is_error) = match decision {
                        PermissionDecision::Deny => (
                            format!("Permission denied by policy for tool '{name}'"),
                            true,
                        ),
                        PermissionDecision::Ask => {
                            let approved = match &self.permission_handler {
                                Some(handler) => handler.request_permission(name, input).await,
                                None => false,
                            };

                            if approved {
                                if let Some(tool) = self.tools.get(name) {
                                    match tool.execute(input.clone(), &self.workspace_root).await {
                                        Ok(res) => (res, false),
                                        Err(err) => (err, true),
                                    }
                                } else {
                                    (format!("Unknown tool '{name}'"), true)
                                }
                            } else {
                                (
                                    format!("Permission error: Tool '{name}' requires user confirmation before execution"),
                                    true,
                                )
                            }
                        }
                        PermissionDecision::Allow => {
                            if let Some(tool) = self.tools.get(name) {
                                match tool.execute(input.clone(), &self.workspace_root).await {
                                    Ok(res) => (res, false),
                                    Err(err) => (err, true),
                                }
                            } else {
                                (format!("Unknown tool '{name}'"), true)
                            }
                        }
                    };

                    if let Some(ref tx) = self.event_sender {
                        let _ = tx.send(agent_core::StreamEvent::StatusUpdate {
                            status: if is_error {
                                format!("{name} ✖ failed")
                            } else {
                                format!("{name} ✔ done")
                            },
                        });
                    }

                    self.session
                        .record_entry(&TranscriptEntry::ToolExecution {
                            tool_name: name.clone(),
                            input: input.clone(),
                            output: output.clone(),
                            is_error,
                        })
                        .await?;

                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output,
                        is_error,
                    });
                }
            }

            self.messages.push(assistant_msg);

            // If no tools were called, the turn is complete
            if tool_results.is_empty() {
                break;
            }

            // Append tool results as a user turn for the next model cycle
            let tool_msg = Message {
                role: Role::User,
                content: tool_results,
            };
            self.messages.push(tool_msg);
        }

        Ok(final_text)
    }

    /// Autonomous repair loop: runs the turn, verifies project test suite, and re-invokes
    /// agent with structured diagnostics if tests fail, iterating until green or max_retries reached.
    pub async fn run_with_autofix(
        &mut self,
        prompt: &str,
        custom_test_cmd: Option<&str>,
        max_retries: usize,
    ) -> Result<crate::autofix::AutoFixReport, String> {
        let initial_response = self.run(prompt).await?;
        let mut history = vec![initial_response.clone()];

        let runner = custom_test_cmd
            .and_then(crate::autofix::parse_custom_test_cmd)
            .or_else(|| crate::autofix::detect_test_runner(&self.workspace_root));

        let Some(runner) = runner else {
            return Ok(crate::autofix::AutoFixReport {
                initial_response,
                attempts: 0,
                final_tests_passed: true,
                history,
            });
        };

        let mut attempts = 0;
        let mut final_tests_passed = false;

        while attempts < max_retries {
            attempts += 1;
            tracing::info!(
                "Running autonomous test verification attempt #{attempts} via {}",
                runner.name
            );
            let test_res = crate::autofix::run_test_suite(&runner, &self.workspace_root)?;
            if test_res.success {
                final_tests_passed = true;
                break;
            }

            let repair_prompt = crate::autofix::format_repair_prompt(&runner, &test_res, attempts);
            let fix_turn_output = self.run(&repair_prompt).await?;
            history.push(fix_turn_output);
        }

        // Final verification pass
        if !final_tests_passed {
            if let Ok(test_res) = crate::autofix::run_test_suite(&runner, &self.workspace_root) {
                final_tests_passed = test_res.success;
            }
        }

        Ok(crate::autofix::AutoFixReport {
            initial_response,
            attempts,
            final_tests_passed,
            history,
        })
    }

    pub fn provider(&self) -> Arc<dyn LlmProvider> {
        self.provider.clone()
    }

    pub fn policy(&self) -> &PermissionPolicy {
        &self.policy
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    pub fn sessions_dir(&self) -> &std::path::Path {
        self.session.sessions_dir()
    }

    pub fn model(&self) -> &str {
        &self.session.metadata().model
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = Some(prompt.into());
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    pub fn set_max_turns(&mut self, turns: u32) {
        self.max_turns = turns;
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    pub fn total_usage(&self) -> &agent_core::TokenUsage {
        &self.session.metadata().total_usage
    }

    pub fn permission_handler(&self) -> Option<Arc<dyn PermissionHandler>> {
        self.permission_handler.clone()
    }

    pub fn set_permission_handler(&mut self, handler: Arc<dyn PermissionHandler>) {
        self.permission_handler = Some(handler);
    }

    pub fn with_permission_handler(mut self, handler: Arc<dyn PermissionHandler>) -> Self {
        self.permission_handler = Some(handler);
        self
    }

    pub fn with_permission_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, &serde_json::Value) -> bool + Send + Sync + 'static,
    {
        self.permission_handler = Some(Arc::new(callback));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        CompletionResponse, EventStream, ModelCapabilities, ProviderError, StreamEvent, TokenUsage,
    };
    use serde_json::json;

    struct ToolCallingMockProvider {
        tool_call_input: serde_json::Value,
    }

    #[async_trait]
    impl LlmProvider for ToolCallingMockProvider {
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
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "call-1".into(),
                        name: "FileWrite".into(),
                        input: self.tool_call_input.clone(),
                    }],
                },
                usage: Some(TokenUsage::default()),
            })
        }
        async fn stream(&self, _req: CompletionRequest) -> Result<EventStream, ProviderError> {
            let events = vec![
                Ok(StreamEvent::MessageStarted {
                    message_id: "msg-1".into(),
                }),
                Ok(StreamEvent::ToolCallStarted {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some("FileWrite".into()),
                }),
                Ok(StreamEvent::ToolCallDelta {
                    index: 0,
                    arguments_chunk: serde_json::to_string(&self.tool_call_input).unwrap(),
                }),
                Ok(StreamEvent::ToolCallCompleted { index: 0 }),
                Ok(StreamEvent::MessageCompleted {
                    usage: Some(TokenUsage::default()),
                }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn test_ask_decision_without_handler_returns_permission_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(ToolCallingMockProvider {
            tool_call_input: json!({
                "file_path": "test.txt",
                "content": "hello world"
            }),
        });

        // Default policy maps mutating actions (like FileWrite) to PermissionDecision::Ask
        let policy = PermissionPolicy::default();
        let mut agent = AgentLoop::new(mock_provider, ws_root, policy, &sessions_dir, "mock-model");
        agent.set_max_turns(1);

        let _ = agent.run("Create test file").await;

        // Verify that the tool was blocked and returned a permission confirmation error
        let messages = agent.messages();
        let mut found_error = false;
        for msg in messages {
            for block in &msg.content {
                if let ContentBlock::ToolResult {
                    content, is_error, ..
                } = block
                {
                    if *is_error
                        && content.contains("Permission error")
                        && content.contains("requires user confirmation")
                    {
                        found_error = true;
                    }
                }
            }
        }
        assert!(
            found_error,
            "Execution should be blocked with user confirmation error when no callback is present"
        );
        assert!(
            !ws_root.join("test.txt").exists(),
            "File should NOT have been created"
        );
    }

    #[tokio::test]
    async fn test_ask_decision_with_approved_handler_executes_tool() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(ToolCallingMockProvider {
            tool_call_input: json!({
                "file_path": "test_approved.txt",
                "content": "approved content"
            }),
        });

        let policy = PermissionPolicy::default();
        let mut agent = AgentLoop::new(mock_provider, ws_root, policy, &sessions_dir, "mock-model")
            .with_permission_callback(|name, _input| {
                // Interactive user confirms FileWrite
                name == "FileWrite"
            });
        agent.set_max_turns(1);

        let _ = agent.run("Create approved file").await;

        assert!(
            ws_root.join("test_approved.txt").exists(),
            "File SHOULD have been created after user approval"
        );
        let content = std::fs::read_to_string(ws_root.join("test_approved.txt")).unwrap();
        assert_eq!(content, "approved content");
    }

    #[tokio::test]
    async fn test_ask_decision_with_denied_handler_blocks_execution() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        let mock_provider = Arc::new(ToolCallingMockProvider {
            tool_call_input: json!({
                "file_path": "test_denied.txt",
                "content": "denied content"
            }),
        });

        let policy = PermissionPolicy::default();
        let mut agent = AgentLoop::new(mock_provider, ws_root, policy, &sessions_dir, "mock-model")
            .with_permission_callback(|_name, _input| {
                // User denies permission
                false
            });
        agent.set_max_turns(1);

        let _ = agent.run("Create denied file").await;

        assert!(
            !ws_root.join("test_denied.txt").exists(),
            "File should NOT have been created when rejected by user"
        );
    }

    #[tokio::test]
    async fn test_agent_loop_microcompacts_earlier_turns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();
        let sessions_dir = temp_dir.path().join("sessions");

        struct EmptyProvider;
        #[async_trait]
        impl LlmProvider for EmptyProvider {
            fn id(&self) -> &str {
                "mock"
            }
            fn name(&self) -> &str {
                "Mock"
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
                    message: Message::assistant_text("done"),
                    usage: None,
                })
            }
            async fn stream(&self, _req: CompletionRequest) -> Result<EventStream, ProviderError> {
                let events = vec![
                    Ok(StreamEvent::TextDelta {
                        text: "done".into(),
                    }),
                    Ok(StreamEvent::MessageCompleted { usage: None }),
                ];
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let policy = PermissionPolicy::default();
        let mut agent = AgentLoop::new(
            Arc::new(EmptyProvider),
            ws_root,
            policy,
            &sessions_dir,
            "mock-model",
        );
        agent.set_max_turns(1);

        // Pre-populate historical turns with a bulky tool result
        agent.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-old".into(),
                content: "Z".repeat(500),
                is_error: false,
            }],
        });
        // Push 2 more turns so the first turn exceeds DEFAULT_KEEP_RECENT_TURNS (2)
        agent.messages.push(Message::user_text("follow up 1"));
        agent.messages.push(Message::assistant_text("response 1"));

        // Running a new prompt triggers compaction
        let _ = agent.run("new prompt").await;

        // Verify the old bulky tool result in message 0 was microcompacted
        if let ContentBlock::ToolResult { content, .. } = &agent.messages[0].content[0] {
            assert_eq!(content, crate::compactor::OLD_TOOL_RESULT_CLEARED_MESSAGE);
        } else {
            panic!("Expected ToolResult at message 0");
        }
    }
}
