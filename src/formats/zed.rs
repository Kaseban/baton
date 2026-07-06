//! Zed AI assistant session format.
//!
//! Zed stores assistant panel conversations as JSON arrays of `Message`:
//!   [
//!     { "User": { "id": "...", "content": [{"Text":"..."}|{"Mention":{...}}|{"Image":{...}}] } },
//!     { "Agent": { "content": [{"Text":"..."}|{"Thinking":{...}}|{"ToolUse":{...}}], "tool_results": {...}, ... } },
//!     { "Resume": null },
//!     { "Compaction": {...} }
//!   ]
//!
//! Storage:
//!   macOS:  ~/Library/Application Support/Zed/conversations/*.json
//!   Linux:  ~/.local/share/zed/conversations/*.json
//!   Windows: %APPDATA%\Zed\conversations\*.json

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Zed;

impl Format for Zed {
    const AGENT: Agent = Agent::Zed;
    const NAME: &'static str = "Zed";

    fn session_dir() -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            dirs::home_dir().map(|h| {
                h.join("Library")
                    .join("Application Support")
                    .join("Zed")
                    .join("conversations")
            })
        } else if cfg!(target_os = "windows") {
            dirs::data_dir().map(|d| d.join("Zed").join("conversations"))
        } else {
            dirs::data_local_dir().map(|d| d.join("zed").join("conversations"))
        };
        base.unwrap_or_else(|| PathBuf::from("."))
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading zed session {}", path.display()))?;
        let items: Vec<ZedMessage> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing zed conversation {}", path.display()))?;

        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let now = chrono::Utc::now().timestamp_millis();
        let mut messages = Vec::new();
        let mut ts = now;

        for item in items {
            let (role, parts) = match item {
                ZedMessage::User(msg) => {
                    let parts: Vec<Part> = msg
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ZedUserContent::Text { text } => {
                                if text.is_empty() {
                                    None
                                } else {
                                    Some(Part::Text { text: text.clone() })
                                }
                            }
                            _ => None,
                        })
                        .collect();
                    (Role::User, parts)
                }
                ZedMessage::Agent(msg) => {
                    let mut parts = Vec::new();
                    for c in &msg.content {
                        match c {
                            ZedAgentContent::Text { text } => {
                                if !text.is_empty() {
                                    parts.push(Part::Text { text: text.clone() });
                                }
                            }
                            ZedAgentContent::Thinking { text, .. } => {
                                if !text.is_empty() {
                                    parts.push(Part::Reasoning { text: text.clone() });
                                }
                            }
                            ZedAgentContent::ToolUse(tool) => {
                                let input = serde_json::to_value(&tool.input).ok();
                                parts.push(Part::ToolCall {
                                    name: tool.name.clone(),
                                    id: Some(tool.id.clone()),
                                    input,
                                });
                            }
                            _ => {}
                        }
                    }
                    (Role::Assistant, parts)
                }
                _ => continue,
            };
            if parts.is_empty() {
                continue;
            }
            messages.push(Message {
                role,
                parts,
                time_created: ts,
                origin: Some(Agent::Zed),
            });
            ts += 1;
        }

        let first_user = messages
            .iter()
            .find(|m| m.role == Role::User)
            .and_then(|m| {
                m.parts.iter().find_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .unwrap_or("");
        let title = if first_user.is_empty() {
            "Zed conversation".to_string()
        } else {
            first_user.chars().take(60).collect()
        };

        Ok(Session {
            source_id: session_id,
            origin: Agent::Zed,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("zed write not implemented yet.")
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
enum ZedMessage {
    User(ZedUserMessage),
    Agent(ZedAgentMessage),
    Resume,
    Compaction(#[allow(dead_code)] serde_json::Value),
}

#[derive(Debug, Deserialize)]
struct ZedUserMessage {
    #[serde(default)]
    content: Vec<ZedUserContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
enum ZedUserContent {
    Text { text: String },
    Mention(#[allow(dead_code)] serde_json::Value),
    Image(#[allow(dead_code)] serde_json::Value),
}

#[derive(Debug, Deserialize)]
struct ZedAgentMessage {
    #[serde(default)]
    content: Vec<ZedAgentContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
enum ZedAgentContent {
    Text { text: String },
    Thinking { text: String, #[allow(dead_code)] signature: Option<String> },
    RedactedThinking(#[allow(dead_code)] String),
    ToolUse(ZedToolUse),
}

#[derive(Debug, Deserialize)]
struct ZedToolUse {
    id: String,
    name: String,
    input: serde_json::Value,
}
