//! Canonical session representation.
//!
//! Every agent format is read into [`Session`] and written from [`Session`].
//! This makes adding a new format O(1) instead of O(N×M) per-pair converters.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Originating agent for a session or message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Opencode,
    Codex,
    Cursor,
    Continue,
    Cline,
    Zed,
    Aider,
    GeminiCli,
    Unknown,
}

impl Agent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Opencode => "opencode",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Continue => "continue",
            Agent::Cline => "cline",
            Agent::Zed => "zed",
            Agent::Aider => "aider",
            Agent::GeminiCli => "gemini-cli",
            Agent::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Some(match s.as_str() {
            "claude" | "claude-code" | "claudecode" => Agent::ClaudeCode,
            "opencode" => Agent::Opencode,
            "codex" => Agent::Codex,
            "cursor" => Agent::Cursor,
            "continue" => Agent::Continue,
            "cline" | "roo" | "roo-code" | "roocode" => Agent::Cline,
            "zed" => Agent::Zed,
            "aider" => Agent::Aider,
            "gemini" | "gemini-cli" => Agent::GeminiCli,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single block of a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Part {
    /// Plain text content from user or assistant.
    Text { text: String },
    /// Reasoning / thinking trace (Claude "thinking", o1 chain-of-thought, etc.).
    Reasoning { text: String },
    /// A tool invocation made by the assistant.
    ToolCall {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// The result of a tool invocation.
    ToolResult {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// A file or image attachment (path or base64).
    Attachment {
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
}

impl Part {
    pub fn text(s: impl Into<String>) -> Self {
        Part::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        if let Part::Text { text } = self {
            Some(text)
        } else {
            None
        }
    }
}

/// A message in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    /// Epoch milliseconds.
    #[serde(default)]
    pub time_created: i64,
    /// Origin agent that produced this message (useful when merging).
    #[serde(default)]
    pub origin: Option<Agent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Canonical session — the intermediate representation every format funnels through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// ID in the *source* agent's namespace (e.g. a Claude `sessionId` UUID).
    pub source_id: String,
    /// Originating agent.
    pub origin: Agent,
    /// Human-readable title (first user prompt truncated, or agent-provided title).
    #[serde(default)]
    pub title: String,
    /// Epoch milliseconds.
    #[serde(default)]
    pub time_created: i64,
    #[serde(default)]
    pub time_updated: i64,
    /// The directory the session was working in, if known.
    #[serde(default)]
    pub directory: Option<String>,
    pub messages: Vec<Message>,
}

impl Session {
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn first_user_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .and_then(|m| {
                m.parts.iter().find_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
    }
}

/// A format codec: read raw → canonical, and write canonical → raw.
pub trait Format {
    const AGENT: Agent;
    /// Human-readable name, e.g. "Claude Code".
    const NAME: &'static str;

    /// Where this agent stores sessions on disk (best-effort, platform-aware).
    fn session_dir() -> std::path::PathBuf;

    /// Read one session by id (or path) into the canonical representation.
    fn read(path: &Path) -> anyhow::Result<Session>;

    /// Write a canonical session out in this agent's format.
    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()>;

    /// List all sessions known to this agent. Returns (id, title, mtime, path).
    fn list() -> Vec<SessionRef> {
        let dir = Self::session_dir();
        if !dir.exists() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if let Some(meta) = entry.metadata().ok() {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let id = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(SessionRef {
                        agent: Self::AGENT,
                        id,
                        title: String::new(),
                        path: p,
                        mtime,
                    });
                }
            }
        }
        out
    }
}

/// Lightweight reference to a session on disk (no content loaded).
#[derive(Debug, Clone)]
pub struct SessionRef {
    pub agent: Agent,
    pub id: String,
    pub title: String,
    pub path: std::path::PathBuf,
    pub mtime: i64,
}
