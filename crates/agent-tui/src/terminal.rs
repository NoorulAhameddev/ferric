use std::io::{self, stdout, Write};
use std::sync::Once;
use std::time::Duration;

use crossterm::{
    cursor::Show,
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::time::interval;

use crate::app::{AppState, AppStatus};

static PANIC_HOOK_SET: Once = Once::new();

/// Installs a panic hook that resets the terminal back to normal mode
/// before printing the panic message, preventing corrupted terminal states.
pub fn install_panic_hook() {
    PANIC_HOOK_SET.call_once(|| {
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = restore();
            original_hook(panic_info);
        }));
    });
}

/// Restores the terminal back to normal mode:
/// disables raw mode, leaves alternate screen buffer, and restores cursor visibility.
pub fn restore() -> io::Result<()> {
    let raw_res = disable_raw_mode();
    let mut out = stdout();
    let _ = execute!(
        out,
        LeaveAlternateScreen,
        crossterm::event::DisableBracketedPaste,
        Show
    );
    out.flush()?;
    let _ = raw_res;
    Ok(())
}

/// Wrapper around a ratatui Terminal that manages raw mode and alternate screen buffer,
/// ensuring clean terminal restoration on drop or explicit call to `restore()`.
pub struct TuiTerminal {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TuiTerminal {
    /// Enters raw mode, switches to the alternate screen buffer,
    /// installs the panic hook, and constructs a new `TuiTerminal`.
    pub fn new() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableBracketedPaste
        ) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(err) => {
                let _ = restore();
                return Err(err);
            }
        };

        Ok(Self {
            terminal,
            restored: false,
        })
    }

    /// Restores the terminal state if not already restored.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.restored {
            self.restored = true;
            restore()?;
        }
        Ok(())
    }

    pub fn inner(&self) -> &Terminal<CrosstermBackend<io::Stdout>> {
        &self.terminal
    }

    pub fn inner_mut(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }
}

impl std::ops::Deref for TuiTerminal {
    type Target = Terminal<CrosstermBackend<io::Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl std::ops::DerefMut for TuiTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for TuiTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Helper function to initialize terminal
pub fn setup_terminal() -> io::Result<TuiTerminal> {
    TuiTerminal::new()
}

/// Handles keyboard events according to agent TUI specifications.
pub fn handle_key_event(app: &mut AppState, key: KeyEvent) {
    // Ignore key release events on Windows
    if key.kind == KeyEventKind::Release {
        return;
    }

    // Permission modal handling takes priority
    if let AppStatus::PermissionPrompt { .. } = &app.status {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(responder) = app.permission_responder.take() {
                    let _ = responder.send(true);
                }
                app.status = AppStatus::Idle;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                if let Some(responder) = app.permission_responder.take() {
                    let _ = responder.send(false);
                }
                app.status = AppStatus::Idle;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Some(responder) = app.permission_responder.take() {
                    let _ = responder.send(true);
                }
                app.status = AppStatus::Idle;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(responder) = app.permission_responder.take() {
                    let _ = responder.send(false);
                }
                app.status = AppStatus::Idle;
            }
            _ => {}
        }
        return;
    }

    // Global cancellation and exit
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if matches!(app.status, AppStatus::Running { .. }) {
            app.status = AppStatus::Idle;
            app.pending_prompt = None;
            app.add_assistant_message("[Operation cancelled by user]".to_string(), Vec::new());
        } else {
            app.should_quit = true;
        }
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('d')
        && app.input_buffer.is_empty()
    {
        app.should_quit = true;
        return;
    }

    if key.code == KeyCode::Tab {
        app.complete_tab();
        return;
    } else {
        app.reset_completion();
    }

    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Enter => {
            let is_newline = key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT)
                || (key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('j'));

            if is_newline {
                app.insert_char('\n');
            } else if !matches!(app.status, AppStatus::Running { .. }) {
                let input = app.take_input();
                let trimmed = input.trim();
                if !trimmed.is_empty() {
                    app.record_history(trimmed);
                    if trimmed.starts_with('/') {
                        app.handle_slash_command(trimmed);
                    } else {
                        let prompt = trimmed.to_string();
                        app.add_user_message(prompt.clone());
                        app.status = AppStatus::Running {
                            step: 1,
                            spinner_frame: 0,
                        };
                        app.scroll_offset = 0;
                        app.pending_prompt = Some(prompt);
                    }
                }
            }
        }
        KeyCode::Backspace => {
            app.delete_char_before_cursor();
        }
        KeyCode::Delete => {
            if app.cursor_pos < app.input_buffer.len() {
                let next_char_len = app.input_buffer[app.cursor_pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(0);
                if next_char_len > 0 {
                    app.input_buffer
                        .drain(app.cursor_pos..app.cursor_pos + next_char_len);
                }
            }
        }
        KeyCode::Left => {
            app.move_cursor_left();
        }
        KeyCode::Right => {
            app.move_cursor_right();
        }
        KeyCode::Home => {
            app.cursor_pos = 0;
        }
        KeyCode::End => {
            app.cursor_pos = app.input_buffer.len();
        }
        KeyCode::Up => {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::CONTROL)
            {
                app.scroll_up(1);
            } else {
                let is_on_first_line = !app.input_buffer[..app.cursor_pos].contains('\n');
                if is_on_first_line {
                    app.history_prev();
                } else {
                    app.move_cursor_up();
                }
            }
        }
        KeyCode::Down => {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::CONTROL)
            {
                app.scroll_down(1);
            } else {
                let is_on_last_line = !app.input_buffer[app.cursor_pos..].contains('\n');
                if is_on_last_line {
                    app.history_next();
                } else {
                    app.move_cursor_down();
                }
            }
        }
        KeyCode::PageUp => {
            app.scroll_up(10);
        }
        KeyCode::PageDown => {
            app.scroll_down(10);
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'u' => {
                        app.input_buffer.drain(..app.cursor_pos);
                        app.cursor_pos = 0;
                    }
                    'k' => {
                        app.input_buffer.truncate(app.cursor_pos);
                    }
                    'a' => {
                        app.cursor_pos = 0;
                    }
                    'e' => {
                        app.cursor_pos = app.input_buffer.len();
                    }
                    'w' => {
                        let text_before = &app.input_buffer[..app.cursor_pos];
                        let trimmed = text_before.trim_end();
                        let new_len = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                        app.input_buffer.drain(new_len..app.cursor_pos);
                        app.cursor_pos = new_len;
                    }
                    _ => {}
                }
            } else {
                app.insert_char(c);
            }
        }
        _ => {}
    }
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Represents the completed result of a single turn execution.
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub text: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<crate::app::UiToolCall>,
    pub total_usage: Option<agent_core::TokenUsage>,
}

impl TurnResult {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            thinking: None,
            tool_calls: Vec::new(),
            total_usage: None,
        }
    }

    pub fn with_thinking(mut self, thinking: Option<String>) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn with_tool_calls(mut self, tool_calls: Vec<crate::app::UiToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    pub fn with_usage(mut self, usage: agent_core::TokenUsage) -> Self {
        self.total_usage = Some(usage);
        self
    }
}

impl From<String> for TurnResult {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for TurnResult {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Execution updates streamed or signaled from the background runner task.
#[derive(Debug)]
pub enum ExecutionUpdate {
    Thinking(String),
    Activity(String),
    Token(String),
    ToolCall(crate::app::UiToolCall),
    PermissionRequest {
        tool_name: String,
        target: String,
        responder: tokio::sync::oneshot::Sender<bool>,
    },
    Complete(TurnResult),
    Error(String),
}

/// Asynchronous runner trait for executing user prompts.
pub trait PromptRunner: Send + Sync + 'static {
    fn run(&self, prompt: String) -> BoxFuture<'static, Result<TurnResult, String>>;
}

impl<F, Fut, R> PromptRunner for F
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<R, String>> + Send + 'static,
    R: Into<TurnResult> + Send + 'static,
{
    fn run(&self, prompt: String) -> BoxFuture<'static, Result<TurnResult, String>> {
        let fut = (self)(prompt);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res.into())
        })
    }
}

impl PromptRunner for std::sync::Arc<dyn PromptRunner> {
    fn run(&self, prompt: String) -> BoxFuture<'static, Result<TurnResult, String>> {
        (**self).run(prompt)
    }
}

async fn run_event_loop_internal<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
    runner: Option<std::sync::Arc<dyn PromptRunner>>,
    prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    external_update_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ExecutionUpdate>>,
) -> io::Result<()> {
    let mut reader = EventStream::new();
    let mut ticker = interval(Duration::from_millis(50));

    let (internal_tx, mut internal_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutionUpdate>();

    let _forward_handle = if let Some(mut ext_rx) = external_update_rx {
        let fwd_tx = internal_tx.clone();
        Some(tokio::spawn(async move {
            while let Some(update) = ext_rx.recv().await {
                if fwd_tx.send(update).is_err() {
                    break;
                }
            }
        }))
    } else {
        None
    };

    let mut active_task: Option<tokio::task::JoinHandle<()>> = None;

    // Render initial frame
    terminal.draw(|f| crate::ui::render(f, app))?;

    while !app.should_quit {
        tokio::select! {
            _ = ticker.tick() => {
                app.tick();
                terminal.draw(|f| crate::ui::render(f, app))?;
            }
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key_event))) => {
                        let is_ctrl_c = key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && (key_event.code == KeyCode::Char('c') || key_event.code == KeyCode::Char('C'));

                        if is_ctrl_c && matches!(app.status, AppStatus::Running { .. }) {
                            if let Some(task) = active_task.take() {
                                task.abort();
                            }
                        }

                        handle_key_event(app, key_event);

                        if app.take_pending_undo() {
                            if let Some(task) = active_task.take() {
                                task.abort();
                            }

                            if let Some(ref tx) = prompt_tx {
                                let _ = tx.send("/undo".to_string());
                            } else if let Some(ref r) = runner {
                                let r_clone = std::sync::Arc::clone(r);
                                let tx_clone = internal_tx.clone();
                                active_task = Some(tokio::spawn(async move {
                                    match r_clone.run("/undo".to_string()).await {
                                        Ok(turn_res) => {
                                            let _ = tx_clone.send(ExecutionUpdate::Complete(turn_res));
                                        }
                                        Err(err) => {
                                            let _ = tx_clone.send(ExecutionUpdate::Error(err));
                                        }
                                    }
                                }));
                            }
                        }

                        if let Some(prompt) = app.take_pending_prompt() {
                            if let Some(task) = active_task.take() {
                                task.abort();
                            }

                            if let Some(ref tx) = prompt_tx {
                                let _ = tx.send(prompt);
                            } else if let Some(ref r) = runner {
                                let r_clone = std::sync::Arc::clone(r);
                                let tx_clone = internal_tx.clone();
                                active_task = Some(tokio::spawn(async move {
                                    match r_clone.run(prompt).await {
                                        Ok(turn_res) => {
                                            let _ = tx_clone.send(ExecutionUpdate::Complete(turn_res));
                                        }
                                        Err(err) => {
                                            let _ = tx_clone.send(ExecutionUpdate::Error(err));
                                        }
                                    }
                                }));
                            } else {
                                app.status = AppStatus::Idle;
                                app.add_assistant_message("[No LLM runner configured]".to_string(), Vec::new());
                            }
                        }
                        terminal.draw(|f| crate::ui::render(f, app))?;
                    }
                    Some(Ok(Event::Paste(pasted_text))) => {
                        app.insert_str(&pasted_text);
                        terminal.draw(|f| crate::ui::render(f, app))?;
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        terminal.draw(|f| crate::ui::render(f, app))?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err),
                    None => break,
                }
            }
            Some(update) = internal_rx.recv() => {
                match update {
                    ExecutionUpdate::Thinking(chunk) => {
                        app.current_thinking.push_str(&chunk);
                    }
                    ExecutionUpdate::Activity(act) => {
                        app.current_activity = Some(act);
                    }
                    ExecutionUpdate::Token(token) => {
                        if let Some(last_msg) = app.messages.last_mut() {
                            if last_msg.role == agent_core::Role::Assistant {
                                last_msg.text.push_str(&token);
                            } else {
                                app.messages.push(crate::app::UiMessage {
                                    role: agent_core::Role::Assistant,
                                    text: token,
                                    thinking: if app.current_thinking.is_empty() {
                                        None
                                    } else {
                                        Some(std::mem::take(&mut app.current_thinking))
                                    },
                                    tool_calls: Vec::new(),
                                });
                            }
                        } else {
                            app.messages.push(crate::app::UiMessage {
                                role: agent_core::Role::Assistant,
                                text: token,
                                thinking: if app.current_thinking.is_empty() {
                                    None
                                } else {
                                    Some(std::mem::take(&mut app.current_thinking))
                                },
                                tool_calls: Vec::new(),
                            });
                        }
                    }
                    ExecutionUpdate::ToolCall(tool_call) => {
                        if let Some(last_msg) = app.messages.last_mut() {
                            if last_msg.role == agent_core::Role::Assistant {
                                last_msg.tool_calls.push(tool_call);
                            } else {
                                app.messages.push(crate::app::UiMessage {
                                    role: agent_core::Role::Assistant,
                                    text: String::new(),
                                    thinking: if app.current_thinking.is_empty() {
                                        None
                                    } else {
                                        Some(std::mem::take(&mut app.current_thinking))
                                    },
                                    tool_calls: vec![tool_call],
                                });
                            }
                        } else {
                            app.messages.push(crate::app::UiMessage {
                                role: agent_core::Role::Assistant,
                                text: String::new(),
                                thinking: if app.current_thinking.is_empty() {
                                    None
                                } else {
                                    Some(std::mem::take(&mut app.current_thinking))
                                },
                                tool_calls: vec![tool_call],
                            });
                        }
                    }
                    ExecutionUpdate::PermissionRequest {
                        tool_name,
                        target,
                        responder,
                    } => {
                        app.prompt_permission(tool_name, target, responder);
                    }
                    ExecutionUpdate::Complete(turn_result) => {
                        active_task = None;
                        app.current_activity = None;
                        let fallback_thinking = if app.current_thinking.is_empty() {
                            None
                        } else {
                            Some(std::mem::take(&mut app.current_thinking))
                        };
                        let resolved_thinking = turn_result.thinking.or(fallback_thinking);

                        if let Some(last_msg) = app.messages.last_mut() {
                            if last_msg.role == agent_core::Role::Assistant {
                                if last_msg.text.is_empty() {
                                    last_msg.text = turn_result.text;
                                }
                                if last_msg.thinking.is_none() {
                                    last_msg.thinking = resolved_thinking;
                                }
                                if last_msg.tool_calls.is_empty() {
                                    last_msg.tool_calls = turn_result.tool_calls;
                                }
                            } else {
                                app.add_assistant_message_with_thinking(
                                    turn_result.text,
                                    resolved_thinking,
                                    turn_result.tool_calls,
                                );
                            }
                        } else {
                            app.add_assistant_message_with_thinking(
                                turn_result.text,
                                resolved_thinking,
                                turn_result.tool_calls,
                            );
                        }

                        if let Some(usage) = turn_result.total_usage {
                            app.total_usage = usage;
                        }

                        app.current_thinking.clear();
                        app.status = AppStatus::Idle;
                    }
                    ExecutionUpdate::Error(err) => {
                        active_task = None;
                        app.status = AppStatus::Error(err);
                    }
                }
                terminal.draw(|f| crate::ui::render(f, app))?;
            }
        }
    }

    if let Some(task) = active_task {
        task.abort();
    }

    Ok(())
}

/// Runs the async event loop without an attached prompt runner.
pub async fn run_event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
) -> io::Result<()> {
    run_event_loop_internal(terminal, app, None, None, None).await
}

/// Runs the async event loop with an attached prompt runner.
pub async fn run_event_loop_with_runner<B, R>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
    runner: R,
) -> io::Result<()>
where
    B: ratatui::backend::Backend,
    R: PromptRunner,
{
    let runner_arc: std::sync::Arc<dyn PromptRunner> = std::sync::Arc::new(runner);
    run_event_loop_internal(terminal, app, Some(runner_arc), None, None).await
}

/// Runs the async event loop with prompt and execution channels.
pub async fn run_event_loop_with_channels<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
    prompt_tx: tokio::sync::mpsc::UnboundedSender<String>,
    update_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutionUpdate>,
) -> io::Result<()> {
    run_event_loop_internal(terminal, app, None, Some(prompt_tx), Some(update_rx)).await
}

/// Runs the full TUI application using Crossterm and Ratatui.
/// Automatically sets up raw mode and alternate screen, installs panic hook,
/// runs the event loop, and cleanly restores the terminal on exit.
pub async fn run_tui(app: &mut AppState) -> io::Result<()> {
    let mut term = TuiTerminal::new()?;
    run_event_loop(&mut term, app).await?;
    term.restore()?;
    Ok(())
}

/// Runs the full TUI application with an attached prompt runner closure or trait implementer.
pub async fn run_tui_with_runner<R>(app: &mut AppState, runner: R) -> io::Result<()>
where
    R: PromptRunner,
{
    let mut term = TuiTerminal::new()?;
    run_event_loop_with_runner(&mut term, app, runner).await?;
    term.restore()?;
    Ok(())
}

/// Runs the full TUI application with an attached prompt runner and an external execution update receiver.
pub async fn run_tui_with_runner_and_updates<R>(
    app: &mut AppState,
    runner: R,
    update_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutionUpdate>,
) -> io::Result<()>
where
    R: PromptRunner,
{
    let mut term = TuiTerminal::new()?;
    let runner_arc: std::sync::Arc<dyn PromptRunner> = std::sync::Arc::new(runner);
    run_event_loop_internal(&mut term, app, Some(runner_arc), None, Some(update_rx)).await?;
    term.restore()?;
    Ok(())
}

/// An interactive permission handler that bridges permission requests from `AgentLoop`
/// into the Ratatui TUI through an unbounded channel, displaying the interactive modal
/// and waiting for user confirmation keystrokes (`y`/`n`/`a`).
#[derive(Clone)]
pub struct TuiPermissionHandler {
    tx: tokio::sync::mpsc::UnboundedSender<ExecutionUpdate>,
}

impl TuiPermissionHandler {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<ExecutionUpdate>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl agent_engine::PermissionHandler for TuiPermissionHandler {
    async fn request_permission(&self, tool_name: &str, input: &serde_json::Value) -> bool {
        let (responder, receiver) = tokio::sync::oneshot::channel();
        let target = input.to_string();
        if self
            .tx
            .send(ExecutionUpdate::PermissionRequest {
                tool_name: tool_name.to_string(),
                target,
                responder,
            })
            .is_err()
        {
            return false;
        }
        receiver.await.unwrap_or(false)
    }
}

/// Runs the full TUI application with prompt and update channels.
pub async fn run_tui_with_channels(
    app: &mut AppState,
    prompt_tx: tokio::sync::mpsc::UnboundedSender<String>,
    update_rx: tokio::sync::mpsc::UnboundedReceiver<ExecutionUpdate>,
) -> io::Result<()> {
    let mut term = TuiTerminal::new()?;
    run_event_loop_with_channels(&mut term, app, prompt_tx, update_rx).await?;
    term.restore()?;
    Ok(())
}
