use agent_core::{Role, TokenUsage};

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Idle,
    Running { step: usize, spinner_frame: usize },
    PermissionPrompt { tool_name: String, target: String },
    Error(String),
}

pub const KNOWN_SLASH_COMMANDS: &[&str] = &[
    "/clear", "/cost", "/usage", "/help", "/plan", "/undo", "/quit", "/exit",
];

#[derive(Debug, Clone)]
pub struct CompletionState {
    pub replacement_start: usize,
    pub candidates: Vec<String>,
    pub current_index: usize,
}

#[derive(Debug, Clone)]
pub struct UiToolCall {
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct UiMessage {
    pub role: Role,
    pub text: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<UiToolCall>,
}

pub struct AppState {
    pub model: String,
    pub provider: String,
    pub input_buffer: String,
    pub cursor_pos: usize,
    pub messages: Vec<UiMessage>,
    pub status: AppStatus,
    pub total_usage: TokenUsage,
    pub scroll_offset: u16,
    pub should_quit: bool,
    pub pending_prompt: Option<String>,
    pub plan_mode: bool,
    pub permission_responder: Option<tokio::sync::oneshot::Sender<bool>>,
    pub current_thinking: String,
    pub current_activity: Option<String>,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub history_draft: String,
    pub history_file: Option<std::path::PathBuf>,
    pub completion: Option<CompletionState>,
    pub pending_undo: bool,
}

impl AppState {
    pub const DEFAULT_HISTORY_CAPACITY: usize = 1000;

    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
            input_buffer: String::new(),
            cursor_pos: 0,
            messages: Vec::new(),
            status: AppStatus::Idle,
            total_usage: TokenUsage::default(),
            scroll_offset: 0,
            should_quit: false,
            pending_prompt: None,
            plan_mode: false,
            permission_responder: None,
            current_thinking: String::new(),
            current_activity: None,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            history_file: None,
            completion: None,
            pending_undo: false,
        }
    }

    pub fn prompt_permission(
        &mut self,
        tool_name: String,
        target: String,
        responder: tokio::sync::oneshot::Sender<bool>,
    ) {
        self.status = AppStatus::PermissionPrompt { tool_name, target };
        self.permission_responder = Some(responder);
    }

    pub fn handle_slash_command(&mut self, input: &str) -> bool {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return false;
        }

        let cmd = trimmed.split_whitespace().next().unwrap_or(trimmed);
        match cmd {
            "/clear" => {
                self.messages.clear();
                self.scroll_offset = 0;
            }
            "/cost" | "/usage" => {
                let text = format!(
                    "Token usage:\n  • Input tokens: {}\n  • Output tokens: {}\n  • Total tokens: {}",
                    self.total_usage.input_tokens,
                    self.total_usage.output_tokens,
                    self.total_usage.input_tokens + self.total_usage.output_tokens
                );
                self.add_assistant_message(text, Vec::new());
            }
            "/help" => {
                let help_text = "\
Available slash commands:
  /clear        - Clear message history and reset scroll
  /cost, /usage - Display current token usage (input and output tokens)
  /help         - Display this directory of commands and shortcuts
  /plan         - Toggle plan mode status notice
  /exit, /quit  - Exit the application

Readline shortcuts:
  Ctrl+A        - Move cursor to beginning of line
  Ctrl+E        - Move cursor to end of line
  Ctrl+U        - Clear line before cursor
  Ctrl+K        - Clear line after cursor
  Ctrl+W        - Delete previous word
  Ctrl+C        - Cancel running operation / Exit when idle
  Ctrl+D        - Exit when input is empty
  Esc           - Exit application
  Up / Down     - Scroll transcript up / down by 1 line
  PageUp / Down - Scroll transcript up / down by 10 lines";
                self.add_assistant_message(help_text.to_string(), Vec::new());
            }
            "/exit" | "/quit" => {
                self.should_quit = true;
            }
            "/undo" => {
                self.pending_undo = true;
                self.add_assistant_message(
                    "Rolling back previous turn changes...".to_string(),
                    Vec::new(),
                );
            }
            "/plan" => {
                self.plan_mode = !self.plan_mode;
                let notice = if self.plan_mode {
                    "Plan mode toggled: ON. Structured planning and approval active."
                } else {
                    "Plan mode toggled: OFF. Normal autonomous execution restored."
                };
                self.add_assistant_message(notice.to_string(), Vec::new());
            }
            _ => {
                let error_text = format!(
                    "Unknown command: '{cmd}'. Type /help for available slash commands and shortcuts."
                );
                self.add_assistant_message(error_text, Vec::new());
            }
        }

        true
    }

    pub fn take_pending_prompt(&mut self) -> Option<String> {
        self.pending_prompt.take()
    }

    pub fn take_pending_undo(&mut self) -> bool {
        std::mem::take(&mut self.pending_undo)
    }

    pub fn insert_char(&mut self, c: char) {
        self.input_buffer.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn delete_char_before_cursor(&mut self) {
        if self.cursor_pos > 0 {
            let prev_idx = self.input_buffer[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input_buffer.remove(prev_idx);
            self.cursor_pos = prev_idx;
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            if let Some((i, _)) = self.input_buffer[..self.cursor_pos].char_indices().last() {
                self.cursor_pos = i;
            }
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input_buffer.len() {
            if let Some((_, c)) = self.input_buffer[self.cursor_pos..].char_indices().next() {
                self.cursor_pos += c.len_utf8();
            }
        }
    }

    pub fn take_input(&mut self) -> String {
        self.cursor_pos = 0;
        std::mem::take(&mut self.input_buffer)
    }

    pub fn tick(&mut self) {
        if let AppStatus::Running {
            step,
            spinner_frame,
        } = self.status
        {
            self.status = AppStatus::Running {
                step,
                spinner_frame: (spinner_frame + 1) % SPINNER_FRAMES.len(),
            };
        }
    }

    pub fn add_user_message(&mut self, text: String) {
        self.messages.push(UiMessage {
            role: Role::User,
            text,
            thinking: None,
            tool_calls: Vec::new(),
        });
    }

    pub fn add_assistant_message(&mut self, text: String, tool_calls: Vec<UiToolCall>) {
        let thinking = if self.current_thinking.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current_thinking))
        };
        self.messages.push(UiMessage {
            role: Role::Assistant,
            text,
            thinking,
            tool_calls,
        });
    }

    pub fn add_assistant_message_with_thinking(
        &mut self,
        text: String,
        thinking: Option<String>,
        tool_calls: Vec<UiToolCall>,
    ) {
        self.messages.push(UiMessage {
            role: Role::Assistant,
            text,
            thinking,
            tool_calls,
        });
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Configures persistent history file, resolving ~/.agent_history
    pub fn with_history_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        let p = path.into();
        self.load_history_file(&p);
        self.history_file = Some(p);
        self
    }

    /// Loads history from disk line-by-line, deduplicating consecutive entries
    pub fn load_history_file(&mut self, path: &std::path::Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && self.history.last().map(|s| s.as_str()) != Some(trimmed) {
                    self.history.push(trimmed.to_string());
                }
            }
        }
    }

    /// Navigates to earlier prompt history (Up arrow)
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // Save unsaved prompt draft
                self.history_draft = self.input_buffer.clone();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.input_buffer = self.history[idx].clone();
                self.cursor_pos = self.input_buffer.len();
            }
            Some(idx) => {
                if idx > 0 {
                    let new_idx = idx - 1;
                    self.history_index = Some(new_idx);
                    self.input_buffer = self.history[new_idx].clone();
                    self.cursor_pos = self.input_buffer.len();
                }
            }
        }
    }

    /// Navigates to later prompt history (Down arrow)
    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) => {
                if idx + 1 < self.history.len() {
                    let new_idx = idx + 1;
                    self.history_index = Some(new_idx);
                    self.input_buffer = self.history[new_idx].clone();
                    self.cursor_pos = self.input_buffer.len();
                } else {
                    // Restores unsubmitted draft
                    self.history_index = None;
                    self.input_buffer = std::mem::take(&mut self.history_draft);
                    self.cursor_pos = self.input_buffer.len();
                }
            }
        }
    }

    /// Appends submitted prompt to history and disk
    pub fn record_history(&mut self, prompt: &str) {
        self.history_index = None;
        self.history_draft.clear();
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return;
        }

        if self.history.last().map(|s| s.as_str()) != Some(trimmed) {
            self.history.push(trimmed.to_string());
            if self.history.len() > Self::DEFAULT_HISTORY_CAPACITY {
                self.history.remove(0);
            }
            if let Some(ref path) = self.history_file {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{trimmed}");
                }
            }
        }
    }

    pub fn reset_completion(&mut self) {
        self.completion = None;
    }

    pub fn complete_tab(&mut self) {
        // 1. If currently cycling through candidates, advance to next
        if let Some(ref mut state) = self.completion {
            if !state.candidates.is_empty() {
                state.current_index = (state.current_index + 1) % state.candidates.len();
                let candidate = &state.candidates[state.current_index];
                self.input_buffer
                    .replace_range(state.replacement_start..self.cursor_pos, candidate);
                self.cursor_pos = state.replacement_start + candidate.len();
                return;
            }
        }

        // 2. Identify word prefix before cursor
        let text_before = &self.input_buffer[..self.cursor_pos];
        if text_before.trim().is_empty() {
            return;
        }

        // Check if completing slash command at line start
        if text_before.starts_with('/') && !text_before[1..].contains(char::is_whitespace) {
            let prefix = text_before;
            let matches: Vec<String> = KNOWN_SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(prefix))
                .map(|cmd| format!("{cmd} "))
                .collect();

            self.apply_completion_matches(0, matches);
            return;
        }

        // File path completion: find word start delimiter
        let word_start = text_before
            .rfind(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = &text_before[word_start..];
        if prefix.is_empty() {
            return;
        }

        let candidates = scan_path_completions(prefix);
        self.apply_completion_matches(word_start, candidates);
    }

    fn apply_completion_matches(&mut self, start: usize, matches: Vec<String>) {
        if matches.is_empty() {
            return;
        }

        if matches.len() == 1 {
            let match_str = &matches[0];
            self.input_buffer
                .replace_range(start..self.cursor_pos, match_str);
            self.cursor_pos = start + match_str.len();
            self.completion = None;
        } else {
            let lcp = longest_common_prefix(&matches);
            let current_len = self.cursor_pos - start;
            if lcp.len() > current_len {
                self.input_buffer
                    .replace_range(start..self.cursor_pos, &lcp);
                self.cursor_pos = start + lcp.len();
            }

            self.completion = Some(CompletionState {
                replacement_start: start,
                candidates: matches,
                current_index: 0,
            });
        }
    }

    pub fn insert_str(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.input_buffer.insert_str(self.cursor_pos, &normalized);
        self.cursor_pos += normalized.len();
        self.reset_completion();
    }

    pub fn move_cursor_up(&mut self) {
        let text_before = &self.input_buffer[..self.cursor_pos];
        if let Some(current_line_start) = text_before.rfind('\n') {
            let col = self.cursor_pos - current_line_start - 1;
            let prev_text = &self.input_buffer[..current_line_start];
            let prev_line_start = prev_text.rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_line_len = current_line_start - prev_line_start;
            self.cursor_pos = prev_line_start + col.min(prev_line_len);
        }
    }

    pub fn move_cursor_down(&mut self) {
        let text_before = &self.input_buffer[..self.cursor_pos];
        let current_line_start = text_before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = self.cursor_pos - current_line_start;

        let text_after = &self.input_buffer[self.cursor_pos..];
        if let Some(rel_nl) = text_after.find('\n') {
            let next_line_start = self.cursor_pos + rel_nl + 1;
            let rest = &self.input_buffer[next_line_start..];
            let next_line_len = rest.find('\n').unwrap_or(rest.len());
            self.cursor_pos = next_line_start + col.min(next_line_len);
        }
    }
}

fn scan_path_completions(prefix: &str) -> Vec<String> {
    use std::path::Path;
    let (search_dir, file_prefix, dir_prefix) = if let Some(last_slash) = prefix.rfind(['/', '\\'])
    {
        let dir_part = &prefix[..=last_slash];
        let file_part = &prefix[last_slash + 1..];
        (Path::new(dir_part), file_part, dir_part)
    } else {
        (Path::new("."), prefix, "")
    };

    let Ok(entries) = std::fs::read_dir(search_dir) else {
        return Vec::new();
    };

    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !file_prefix.starts_with('.') && name.starts_with('.') {
            continue;
        }
        if name.starts_with(file_prefix) {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                matches.push(format!("{dir_prefix}{name}/"));
            } else {
                matches.push(format!("{dir_prefix}{name} "));
            }
        }
    }
    matches.sort();
    matches
}

fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let mut len = first.len();
    for s in &strings[1..] {
        len = len.min(s.len());
        while !s.starts_with(&first[..len]) {
            len -= 1;
        }
        if len == 0 {
            break;
        }
    }
    first[..len].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_input_editing() {
        let mut app = AppState::new("ferric-pro", "anthropic");
        app.insert_char('H');
        app.insert_char('i');
        assert_eq!(app.input_buffer, "Hi");
        assert_eq!(app.cursor_pos, 2);

        app.move_cursor_left();
        app.insert_char('o');
        assert_eq!(app.input_buffer, "Hoi");

        app.delete_char_before_cursor();
        assert_eq!(app.input_buffer, "Hi");

        let submitted = app.take_input();
        assert_eq!(submitted, "Hi");
        assert!(app.input_buffer.is_empty());
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_spinner_animation_tick() {
        let mut app = AppState::new("gpt-4o", "openai");
        app.status = AppStatus::Running {
            step: 1,
            spinner_frame: 0,
        };
        app.tick();
        assert_eq!(
            app.status,
            AppStatus::Running {
                step: 1,
                spinner_frame: 1
            }
        );
    }
}
