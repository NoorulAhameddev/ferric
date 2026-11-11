pub mod app;
pub mod terminal;
pub mod ui;

pub use app::{AppState, AppStatus, UiMessage, UiToolCall, SPINNER_FRAMES};
pub use terminal::{
    handle_key_event, install_panic_hook, restore, run_event_loop, run_event_loop_with_channels,
    run_event_loop_with_runner, run_tui, run_tui_with_channels, run_tui_with_runner,
    run_tui_with_runner_and_updates, setup_terminal, BoxFuture, ExecutionUpdate, PromptRunner,
    TuiPermissionHandler, TuiTerminal, TurnResult,
};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Headless rendering helper for testing and headless verification.
/// Renders the current AppState into a `TestBackend` with the specified dimensions.
pub fn render_headless(app: &AppState, width: u16, height: u16) -> TestBackend {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create TestBackend terminal");
    terminal
        .draw(|f| ui::render(f, app))
        .expect("failed to render frame");
    terminal.backend().clone()
}

/// Converts a `TestBackend`'s screen buffer into a visual string representation.
pub fn backend_to_string(backend: &TestBackend) -> String {
    format!("{backend}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{Role, TokenUsage};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_headless_render_initial_welcome_view() {
        let app = AppState::new("ferric-pro", "anthropic");
        let backend = render_headless(&app, 100, 30);
        let screen = backend_to_string(&backend);

        // Header elements
        assert!(
            screen.contains("Ferric Agent"),
            "Header must show app title"
        );
        assert!(screen.contains("[ferric-pro]"), "Header must show model");
        assert!(screen.contains("(anthropic)"), "Header must show provider");
        assert!(screen.contains("● Idle"), "Header must show Idle status");
        assert!(screen.contains("Tokens: 0"), "Header must show token count");

        // Messages area welcome text
        assert!(
            screen.contains("Welcome to Ferric Agent (Universal Rust Edition)!"),
            "Empty messages should show welcome banner"
        );
        assert!(
            screen.contains("Type a coding prompt below and press Enter"),
            "Empty messages should show instructions"
        );

        // Input prompt box
        assert!(
            screen.contains("Prompt"),
            "Input area must have Prompt title"
        );
    }

    #[test]
    fn test_headless_render_transcript_with_tools() {
        let mut app = AppState::new("gpt-4o", "openai");
        app.total_usage = TokenUsage {
            input_tokens: 120,
            output_tokens: 80,
            ..Default::default()
        };

        app.add_user_message("Refactor the database queries in db.rs".to_string());
        app.add_assistant_message(
            "I will examine the db.rs file first.".to_string(),
            vec![
                UiToolCall {
                    name: "read_file".to_string(),
                    input: "{\"path\": \"src/db.rs\"}".to_string(),
                    output: Some("pub fn query() -> Result<()> {\n    Ok(())\n}".to_string()),
                    is_error: false,
                },
                UiToolCall {
                    name: "shell".to_string(),
                    input: "{\"cmd\": \"cargo test\"}".to_string(),
                    output: Some("error: build failed".to_string()),
                    is_error: true,
                },
            ],
        );

        let backend = render_headless(&app, 120, 35);
        let screen = backend_to_string(&backend);

        // Verify tokens count sum (120 + 80 = 200)
        assert!(screen.contains("Tokens: 200"));

        // Verify conversation transcript
        assert!(screen.contains("You:"));
        assert!(screen.contains("Refactor the database queries in db.rs"));
        assert!(screen.contains("Ferric:"));
        assert!(screen.contains("I will examine the db.rs file first."));

        // Verify tool calls rendered with status icons
        assert!(screen.contains("[read_file]"));
        assert!(screen.contains("{\"path\": \"src/db.rs\"}"));
        assert!(screen.contains("pub fn query() -> Result<()>"));
        assert!(screen.contains("[shell]"));
        assert!(screen.contains("error: build failed"));
    }

    #[test]
    fn test_headless_render_running_status_and_input() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.status = AppStatus::Running {
            step: 3,
            spinner_frame: 2,
        };
        app.input_buffer = "cargo check".to_string();
        app.cursor_pos = 11;

        let backend = render_headless(&app, 100, 25);
        let screen = backend_to_string(&backend);

        // Header should show thinking with step
        assert!(screen.contains("Thinking (Step 3)"));
        // Input box should show the text
        assert!(screen.contains("cargo check"));
    }

    #[test]
    fn test_headless_render_error_status() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.status = AppStatus::Error("API key invalid".to_string());

        let backend = render_headless(&app, 100, 25);
        let screen = backend_to_string(&backend);

        assert!(screen.contains("Error: API key invalid"));
    }

    #[test]
    fn test_headless_render_permission_modal() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.status = AppStatus::PermissionPrompt {
            tool_name: "file_write".to_string(),
            target: "src/main.rs".to_string(),
        };

        let backend = render_headless(&app, 100, 30);
        let screen = backend_to_string(&backend);

        // Header warning
        assert!(screen.contains("Permission Required"));

        // Modal content
        assert!(screen.contains("Permission Request"));
        assert!(screen.contains("Tool Action Requires Approval"));
        assert!(screen.contains("Tool:   file_write"));
        assert!(screen.contains("Action: src/main.rs"));
        assert!(screen.contains("[y] Allow Once"));
        assert!(screen.contains("[n] Deny"));
        assert!(screen.contains("[a] Always Allow"));
    }

    #[test]
    fn test_keyboard_typing_and_navigation() {
        let mut app = AppState::new("test-model", "test-prov");

        // Type "hello"
        for c in "hello".chars() {
            handle_key_event(&mut app, make_key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input_buffer, "hello");
        assert_eq!(app.cursor_pos, 5);

        // Left arrow
        handle_key_event(&mut app, make_key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 4);

        // Insert 'x'
        handle_key_event(&mut app, make_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.input_buffer, "hellxo");
        assert_eq!(app.cursor_pos, 5);

        // Backspace removes 'x'
        handle_key_event(&mut app, make_key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input_buffer, "hello");
        assert_eq!(app.cursor_pos, 4);

        // Delete removes 'o'
        handle_key_event(&mut app, make_key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.input_buffer, "hell");

        // Home key moves cursor to 0
        handle_key_event(&mut app, make_key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 0);

        // End key moves cursor to end
        handle_key_event(&mut app, make_key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 4);
    }

    #[test]
    fn test_keyboard_enter_submission() {
        let mut app = AppState::new("test-model", "test-prov");
        app.input_buffer = "fix unit tests".to_string();
        app.cursor_pos = 14;

        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.input_buffer.is_empty());
        assert_eq!(app.cursor_pos, 0);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, Role::User);
        assert_eq!(app.messages[0].text, "fix unit tests");
        assert_eq!(
            app.status,
            AppStatus::Running {
                step: 1,
                spinner_frame: 0
            }
        );
    }

    #[test]
    fn test_keyboard_cancellation_and_exit() {
        let mut app = AppState::new("test-model", "test-prov");

        // Esc exits app when Idle
        assert!(!app.should_quit);
        handle_key_event(&mut app, make_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.should_quit);

        // Ctrl+C exits app when Idle
        app.should_quit = false;
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(app.should_quit);

        // Ctrl+C while Running cancels running task instead of exiting
        app.should_quit = false;
        app.status = AppStatus::Running {
            step: 2,
            spinner_frame: 4,
        };
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(!app.should_quit);
        assert_eq!(app.status, AppStatus::Idle);
        assert!(app.messages.last().unwrap().text.contains("cancelled"));
    }

    #[test]
    fn test_keyboard_permission_modal_handling() {
        let mut app = AppState::new("test-model", "test-prov");
        app.status = AppStatus::PermissionPrompt {
            tool_name: "shell".to_string(),
            target: "rm -rf target".to_string(),
        };

        // 'y' resolves modal back to idle
        handle_key_event(&mut app, make_key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);

        // Test denial with 'n'
        app.status = AppStatus::PermissionPrompt {
            tool_name: "shell".to_string(),
            target: "cargo run".to_string(),
        };
        handle_key_event(&mut app, make_key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);

        // Test always allow with 'a'
        app.status = AppStatus::PermissionPrompt {
            tool_name: "shell".to_string(),
            target: "cargo run".to_string(),
        };
        handle_key_event(&mut app, make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);

        // Test Esc dismissing modal
        app.status = AppStatus::PermissionPrompt {
            tool_name: "shell".to_string(),
            target: "cargo run".to_string(),
        };
        handle_key_event(&mut app, make_key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_keyboard_scrolling() {
        let mut app = AppState::new("test-model", "test-prov");
        assert_eq!(app.scroll_offset, 0);

        // Up arrow scrolls up
        handle_key_event(&mut app, make_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 1);

        // PageUp scrolls up by 10
        handle_key_event(&mut app, make_key(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 11);

        // Down arrow scrolls down
        handle_key_event(&mut app, make_key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 10);

        // PageDown scrolls down by 10
        handle_key_event(&mut app, make_key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_keyboard_readline_shortcuts() {
        let mut app = AppState::new("test-model", "test-prov");
        app.input_buffer = "git commit -m message".to_string();
        app.cursor_pos = app.input_buffer.len();

        // Ctrl+W deletes previous word
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.input_buffer, "git commit -m ");
        assert_eq!(app.cursor_pos, 14);

        // Ctrl+A moves to start
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('a'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.cursor_pos, 0);

        // Ctrl+E moves to end
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('e'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.cursor_pos, 14);

        // Ctrl+U clears line before cursor
        handle_key_event(
            &mut app,
            make_key(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.input_buffer, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_panic_hook_installation() {
        install_panic_hook();
        // Calling it twice shouldn't panic (idempotent via Once)
        install_panic_hook();
    }

    #[tokio::test]
    async fn test_turn_result_construction_and_conversion() {
        let tr1 = TurnResult::new("Simple response");
        assert_eq!(tr1.text, "Simple response");
        assert!(tr1.tool_calls.is_empty());
        assert!(tr1.total_usage.is_none());

        let tr2: TurnResult = "Converted from str".into();
        assert_eq!(tr2.text, "Converted from str");

        let tr3: TurnResult = "Converted from string".to_string().into();
        assert_eq!(tr3.text, "Converted from string");

        let tr4 = TurnResult::new("With tools and usage")
            .with_tool_calls(vec![UiToolCall {
                name: "test_tool".into(),
                input: "{}".into(),
                output: Some("ok".into()),
                is_error: false,
            }])
            .with_usage(TokenUsage {
                input_tokens: 50,
                output_tokens: 25,
                ..Default::default()
            });
        assert_eq!(tr4.tool_calls.len(), 1);
        assert_eq!(
            tr4.total_usage.as_ref().unwrap().input_tokens
                + tr4.total_usage.as_ref().unwrap().output_tokens,
            75
        );
    }

    #[tokio::test]
    async fn test_prompt_runner_closure_support() {
        // Test simple closure returning Result<String, String>
        let runner_simple = |prompt: String| async move { Ok(format!("Echo: {prompt}")) };
        let res = runner_simple.run("hello".to_string()).await.unwrap();
        assert_eq!(res.text, "Echo: hello");

        // Test closure returning Result<TurnResult, String>
        let runner_turn = |prompt: String| async move {
            Ok(
                TurnResult::new(format!("Echo: {prompt}")).with_usage(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                }),
            )
        };
        let res2 = runner_turn.run("world".to_string()).await.unwrap();
        assert_eq!(res2.text, "Echo: world");
        assert_eq!(res2.total_usage.unwrap().input_tokens, 10);

        // Test error output
        let runner_err = |_prompt: String| async move {
            Err::<String, String>("Rate limit exceeded".to_string())
        };
        let err_res = runner_err.run("fail".to_string()).await;
        assert_eq!(err_res.unwrap_err(), "Rate limit exceeded");
    }

    #[test]
    fn test_pending_prompt_lifecycle() {
        let mut app = AppState::new("test-model", "test-prov");
        assert!(app.pending_prompt.is_none());

        app.input_buffer = "my pending query".to_string();
        app.cursor_pos = app.input_buffer.len();

        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.pending_prompt.as_deref(), Some("my pending query"));
        assert_eq!(
            app.take_pending_prompt().as_deref(),
            Some("my pending query")
        );
        assert!(app.pending_prompt.is_none());
    }

    #[test]
    fn test_slash_commands_clear() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.add_user_message("Hello".to_string());
        app.add_assistant_message("Hi there".to_string(), Vec::new());
        app.scroll_offset = 15;
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.scroll_offset, 15);

        app.input_buffer = "/clear".to_string();
        app.cursor_pos = 6;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.messages.is_empty());
        assert_eq!(app.scroll_offset, 0);
        assert!(app.input_buffer.is_empty());
        assert_eq!(app.cursor_pos, 0);
        assert!(app.pending_prompt.is_none());
        assert_eq!(app.status, AppStatus::Idle);
    }

    #[test]
    fn test_slash_commands_cost_and_usage() {
        let mut app = AppState::new("gpt-4o", "openai");
        app.total_usage = TokenUsage {
            input_tokens: 350,
            output_tokens: 150,
            ..Default::default()
        };

        // Test /cost
        app.input_buffer = "/cost".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, Role::Assistant);
        assert!(app.messages[0].text.contains("350"));
        assert!(app.messages[0].text.contains("150"));
        assert!(app.messages[0].text.contains("500"));
        assert!(app.messages[0].text.contains("Input tokens"));
        assert!(app.messages[0].text.contains("Output tokens"));
        assert!(app.input_buffer.is_empty());
        assert!(app.pending_prompt.is_none());

        // Test /usage
        app.input_buffer = "/usage".to_string();
        app.cursor_pos = 6;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[1].role, Role::Assistant);
        assert!(app.messages[1].text.contains("350"));
        assert!(app.messages[1].text.contains("150"));
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_commands_help() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.input_buffer = "/help".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.messages.len(), 1);
        let help_text = &app.messages[0].text;

        // Verify slash commands listed
        assert!(help_text.contains("/clear"));
        assert!(help_text.contains("/cost"));
        assert!(help_text.contains("/help"));
        assert!(help_text.contains("/plan"));
        assert!(help_text.contains("/exit"));

        // Verify readline shortcuts listed
        assert!(help_text.contains("Ctrl+A"));
        assert!(help_text.contains("Ctrl+E"));
        assert!(help_text.contains("Ctrl+U"));
        assert!(help_text.contains("Ctrl+K"));
        assert!(help_text.contains("Ctrl+W"));
        assert!(help_text.contains("Ctrl+C"));
        assert!(help_text.contains("Esc"));
        assert!(help_text.contains("Up / Down"));
    }

    #[test]
    fn test_slash_commands_exit_and_quit() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        assert!(!app.should_quit);

        app.input_buffer = "/exit".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.should_quit);
        assert!(app.input_buffer.is_empty());
        assert!(app.pending_prompt.is_none());

        app.should_quit = false;
        app.input_buffer = "/quit".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn test_slash_commands_plan() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        assert!(!app.plan_mode);

        // Toggle plan mode ON
        app.input_buffer = "/plan".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.plan_mode);
        assert_eq!(app.messages.len(), 1);
        assert!(app.messages[0].text.contains("Plan mode toggled"));
        assert!(app.messages[0].text.contains("ON"));

        // Toggle plan mode OFF
        app.input_buffer = "/plan".to_string();
        app.cursor_pos = 5;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!app.plan_mode);
        assert_eq!(app.messages.len(), 2);
        assert!(app.messages[1].text.contains("Plan mode toggled"));
        assert!(app.messages[1].text.contains("OFF"));
    }

    #[test]
    fn test_slash_commands_unknown() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.input_buffer = "/unknown_foo".to_string();
        app.cursor_pos = 12;
        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.messages.len(), 1);
        let error_msg = &app.messages[0].text;
        assert!(error_msg.contains("Unknown command"));
        assert!(error_msg.contains("/unknown_foo"));
        assert!(error_msg.contains("/help"));
        assert!(app.input_buffer.is_empty());
        assert!(app.pending_prompt.is_none());
    }

    #[test]
    fn test_slash_commands_skip_llm_runner() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.input_buffer = "/cost".to_string();
        app.cursor_pos = 5;

        handle_key_event(&mut app, make_key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.input_buffer.is_empty());
        assert_eq!(app.cursor_pos, 0);
        assert!(app.pending_prompt.is_none());
        assert_eq!(app.status, AppStatus::Idle);
    }

    #[test]
    fn test_slash_commands_headless_render_plan_mode() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.plan_mode = true;
        let backend = render_headless(&app, 100, 25);
        let screen = backend_to_string(&backend);

        assert!(
            screen.contains("[PLAN]"),
            "Header should render [PLAN] badge when plan mode is active"
        );
    }

    #[tokio::test]
    async fn test_tui_permission_handler_roundtrip_approval_and_denial() {
        use agent_engine::PermissionHandler;
        let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = TuiPermissionHandler::new(update_tx);

        // 1. Test Approval via 'y'
        let handler_clone = handler.clone();
        let prompt_task = tokio::spawn(async move {
            handler_clone
                .request_permission("Shell", &serde_json::json!({ "command": "cargo test" }))
                .await
        });

        // Receive update in TUI
        let update = update_rx.recv().await.expect("Expected ExecutionUpdate");
        let mut app = AppState::new("test-model", "test-provider");
        if let ExecutionUpdate::PermissionRequest {
            tool_name,
            target,
            responder,
        } = update
        {
            app.prompt_permission(tool_name, target, responder);
        } else {
            panic!("Expected PermissionRequest");
        }

        assert!(matches!(app.status, AppStatus::PermissionPrompt { .. }));
        // Simulate user pressing 'y'
        handle_key_event(&mut app, make_key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);
        let result = prompt_task.await.unwrap();
        assert!(result, "Expected request_permission to return true on 'y'");

        // 2. Test Denial via 'n'
        let prompt_task_deny = tokio::spawn(async move {
            handler
                .request_permission("FileEdit", &serde_json::json!({ "file_path": "foo.rs" }))
                .await
        });

        let update_deny = update_rx.recv().await.expect("Expected ExecutionUpdate");
        if let ExecutionUpdate::PermissionRequest {
            tool_name,
            target,
            responder,
        } = update_deny
        {
            app.prompt_permission(tool_name, target, responder);
        } else {
            panic!("Expected PermissionRequest");
        }

        // Simulate user pressing 'n'
        handle_key_event(&mut app, make_key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.status, AppStatus::Idle);
        let result_deny = prompt_task_deny.await.unwrap();
        assert!(
            !result_deny,
            "Expected request_permission to return false on 'n'"
        );
    }
}
