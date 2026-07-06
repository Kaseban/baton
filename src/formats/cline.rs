//! Cline / Roo Code session format.
//!
//! Cline stores task conversation history at:
//!   `<globalStorage>/tasks/<task-id>/api_conversation_history.json`
//!
//! The file is a JSON array of Anthropic API `MessageParam` objects:
//!   [{ "role": "user"|"assistant", "content": "<string>" | [<content blocks>] }]
//!
//! Content blocks follow the Anthropic API shape:
//!   - { "type": "text", "text": "..." }
//!   - { "type": "tool_use", "name": "...", "input": {...} }
//!   - { "type": "tool_result", "content": "...", "is_error": false }
//!   - { "type": "image", "source": {...} }
//!
//! The global storage path varies by platform:
//!   macOS:  ~/Library/Application Support/Code/User/globalStorage/<extension-id>
//!   Linux:  ~/.config/Code/User/globalStorage/<extension-id>
//!   Windows: %APPDATA%\Code\User\globalStorage\<extension-id>
//!
//! Extension IDs: `saoudrizwan.claude-dev` (Cline), `rooveterinaryinc.roo-cline` (Roo Code).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Cline;

impl Format for Cline {
    const AGENT: Agent = Agent::Cline;
    const NAME: &'static str = "Cline / Roo Code";

    fn session_dir() -> PathBuf {
        // Best-effort: look for VS Code global storage for Cline
        let base = if cfg!(target_os = "macos") {
            dirs::home_dir().map(|h| {
                h.join("Library")
                    .join("Application Support")
                    .join("Code")
                    .join("User")
                    .join("globalStorage")
            })
        } else if cfg!(target_os = "windows") {
            dirs::config_dir().map(|c| c.join("Code").join("User").join("globalStorage"))
        } else {
            dirs::config_dir().map(|c| c.join("Code").join("User").join("globalStorage"))
        };

        if let Some(b) = base {
            // Prefer Cline, fall back to Roo Code
            let cline = b.join("saoudrizwan.claude-dev").join("tasks");
            if cline.exists() {
                return cline;
            }
            let roo = b.join("rooveterinaryinc.roo-cline").join("tasks");
            if roo.exists() {
                return roo;
            }
            return b.join("saoudrizwan.claude-dev").join("tasks");
        }
        PathBuf::from(".cline")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading cline session {}", path.display()))?;
        let history: Vec<ClineMessage> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing cline history {}", path.display()))?;

        let session_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let now = chrono::Utc::now().timestamp_millis();
        let mut messages = Vec::new();
        let mut ts = now;

        for msg in history {
            let role = match msg.role.as_str() {
                "assistant" => Role::Assistant,
                "user" => Role::User,
                _ => continue,
            };
            let parts = parse_content(&msg.content);
            if parts.is_empty() {
                continue;
            }
            messages.push(Message {
                role,
                parts,
                time_created: ts,
                origin: Some(Agent::Cline),
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
            "Cline task".to_string()
        } else {
            first_user.chars().take(60).collect()
        };

        Ok(Session {
            source_id: session_id,
            origin: Agent::Cline,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cline write not implemented yet.")
    }
}

#[derive(Debug, Deserialize)]
struct ClineMessage {
    role: String,
    #[serde(default)]
    content: serde_json::Value,
}

fn parse_content(content: &serde_json::Value) -> Vec<Part> {
    let mut parts = Vec::new();
    match content {
        serde_json::Value::String(s) => {
            if !s.is_empty() {
                parts.push(Part::Text { text: s.clone() });
            }
        }
        serde_json::Value::Array(arr) => {
            for block in arr {
                if let Some(btype) = block.get("type").and_then(|v| v.as_str()) {
                    match btype {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    parts.push(Part::Text { text: text.to_string() });
                                }
                            }
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let input = block.get("input").cloned();
                            parts.push(Part::ToolCall { name, input });
                        }
                        "tool_result" => {
                            let output = match block.get("content") {
                                Some(serde_json::Value::String(s)) => Some(s.clone()),
                                Some(other) => Some(other.to_string()),
                                None => None,
                            };
                            let is_error = block.get("is_error").and_then(|v| v.as_bool());
                            parts.push(Part::ToolResult {
                                name: "tool".into(),
                                output,
                                is_error,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
    parts
}
