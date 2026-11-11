use agent_core::{Message, TokenUsage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    pub total_usage: TokenUsage,
    pub turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEntry {
    Message {
        message: Message,
    },
    ToolExecution {
        tool_name: String,
        input: serde_json::Value,
        output: String,
        is_error: bool,
    },
    TurnSummary {
        turn: u32,
        usage: Option<TokenUsage>,
    },
}

pub struct SessionManager {
    sessions_dir: PathBuf,
    current_session: SessionMetadata,
}

impl SessionManager {
    pub fn new(sessions_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        Self {
            sessions_dir: sessions_dir.into(),
            current_session: SessionMetadata {
                session_id,
                title: "New Session".into(),
                created_at: now,
                updated_at: now,
                model: model.into(),
                total_usage: TokenUsage::default(),
                turn_count: 0,
            },
        }
    }

    pub fn session_id(&self) -> &str {
        &self.current_session.session_id
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.current_session
    }

    pub fn sessions_dir(&self) -> &std::path::Path {
        &self.sessions_dir
    }

    /// Appends a transcript entry to `<sessionId>.jsonl`
    pub async fn record_entry(&mut self, entry: &TranscriptEntry) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.sessions_dir)
            .await
            .map_err(|e| format!("Failed to create sessions directory: {e}"))?;

        let transcript_path = self
            .sessions_dir
            .join(format!("{}.jsonl", self.current_session.session_id));
        let serialized = serde_json::to_string(entry).map_err(|e| e.to_string())?;

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
            .await
            .map_err(|e| format!("Failed to open transcript file: {e}"))?;

        file.write_all(format!("{serialized}\n").as_bytes())
            .await
            .map_err(|e| format!("Failed to write transcript: {e}"))?;

        self.current_session.updated_at = Utc::now();
        self.save_metadata().await?;
        Ok(())
    }

    /// Records token usage and increments turn counter
    pub async fn record_turn_usage(&mut self, usage: Option<&TokenUsage>) -> Result<(), String> {
        self.current_session.turn_count += 1;
        if let Some(u) = usage {
            self.current_session.total_usage.input_tokens += u.input_tokens;
            self.current_session.total_usage.output_tokens += u.output_tokens;
            if let Some(c) = u.cache_creation_input_tokens {
                let current = self
                    .current_session
                    .total_usage
                    .cache_creation_input_tokens
                    .unwrap_or(0);
                self.current_session.total_usage.cache_creation_input_tokens = Some(current + c);
            }
            if let Some(r) = u.cache_read_input_tokens {
                let current = self
                    .current_session
                    .total_usage
                    .cache_read_input_tokens
                    .unwrap_or(0);
                self.current_session.total_usage.cache_read_input_tokens = Some(current + r);
            }
        }
        self.save_metadata().await
    }

    async fn save_metadata(&self) -> Result<(), String> {
        let meta_path = self
            .sessions_dir
            .join(format!("{}.meta.json", self.current_session.session_id));
        let json_str =
            serde_json::to_string_pretty(&self.current_session).map_err(|e| e.to_string())?;
        tokio::fs::write(&meta_path, json_str)
            .await
            .map_err(|e| format!("Failed to save session metadata: {e}"))?;
        Ok(())
    }

    /// Constructs a SessionManager managing an existing session
    pub fn from_existing(sessions_dir: impl Into<PathBuf>, metadata: SessionMetadata) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
            current_session: metadata,
        }
    }

    /// Lists all saved sessions sorted by updated_at descending
    pub fn list_sessions(sessions_dir: &std::path::Path) -> Result<Vec<SessionMetadata>, String> {
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let read_dir = std::fs::read_dir(sessions_dir)
            .map_err(|e| format!("Failed to read sessions dir: {e}"))?;

        let mut list = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.to_string_lossy().ends_with(".meta.json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(meta) = serde_json::from_str::<SessionMetadata>(&content) {
                        list.push(meta);
                    }
                }
            }
        }
        list.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(list)
    }

    /// Finds a session by full ID or prefix match
    pub fn find_session(
        sessions_dir: &std::path::Path,
        id_or_prefix: &str,
    ) -> Result<SessionMetadata, String> {
        let sessions = Self::list_sessions(sessions_dir)?;
        let matches: Vec<_> = sessions
            .into_iter()
            .filter(|s| s.session_id.starts_with(id_or_prefix))
            .collect();

        match matches.len() {
            0 => Err(format!("No session found matching prefix '{id_or_prefix}'")),
            1 => Ok(matches.into_iter().next().unwrap()),
            count => Err(format!(
                "Ambiguous session prefix '{id_or_prefix}', matched {count} sessions"
            )),
        }
    }

    /// Returns the most recently updated session
    pub fn latest_session(
        sessions_dir: &std::path::Path,
    ) -> Result<Option<SessionMetadata>, String> {
        let list = Self::list_sessions(sessions_dir)?;
        Ok(list.into_iter().next())
    }

    /// Loads full transcript messages from `<session_id>.jsonl`
    pub fn load_transcript(
        sessions_dir: &std::path::Path,
        session_id: &str,
    ) -> Result<Vec<Message>, String> {
        let transcript_path = sessions_dir.join(format!("{session_id}.jsonl"));
        if !transcript_path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&transcript_path)
            .map_err(|e| format!("Failed to read transcript: {e}"))?;

        let mut messages = Vec::new();
        for line in content.lines() {
            if let Ok(TranscriptEntry::Message { message }) =
                serde_json::from_str::<TranscriptEntry>(line)
            {
                messages.push(message);
            }
        }
        Ok(messages)
    }

    /// Loads metadata and reconstructed message history
    pub fn load_session(
        sessions_dir: &std::path::Path,
        id_or_prefix: &str,
    ) -> Result<(SessionMetadata, Vec<Message>), String> {
        let meta = Self::find_session(sessions_dir, id_or_prefix)?;
        let messages = Self::load_transcript(sessions_dir, &meta.session_id)?;
        Ok((meta, messages))
    }
}
