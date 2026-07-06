//! Continue.dev session format.
//!
//! Continue stores sessions as individual JSON files under `~/.continue/sessions/<uuid>.json`:
//!   {
//!     "sessionId": "...",
//!     "workspace": { "paths": [...] },
//!     "messages": [
//!       { "role": "user", "content": "..." | [<blocks>] },
//!       { "role": "assistant", "content": "..." | [<blocks>] }
//!     ]
//!   }
//!
//! Content blocks follow the Anthropic/OpenAI content shape:
//!   - { "type": "text", "text": "..." }
//!   - { "type": "tool_call", "name": "...", "input": {...} }
//!   - { "type": "image", "source": {...} }

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct ContinueDev;

impl Format for ContinueDev {
    const AGENT: Agent = Agent::Continue;
    const NAME: &'static str = "Continue";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".continue")
            .join("sessions")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading continue session {}", path.display()))?;
        let file: ContinueSession = serde_json::from_str(&raw)
            .with_context(|| format!("parsing continue session {}", path.display()))?;

        let session_id = file.session_id.clone();
        let now = chrono::Utc::now().timestamp_millis();
        let mut messages = Vec::new();
        let mut ts = now;

        for msg in file.messages {
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
                origin: Some(Agent::Continue),
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
            "Continue session".to_string()
        } else {
            first_user.chars().take(60).collect()
        };

        Ok(Session {
            source_id: session_id,
            origin: Agent::Continue,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("continue write not implemented yet.")
    }
}

#[derive(Debug, Deserialize)]
struct ContinueSession {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    messages: Vec<ContinueMessage>,
}

#[derive(Debug, Deserialize)]
struct ContinueMessage {
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
                        "tool_call" | "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let input = block.get("input").cloned();
                            parts.push(Part::ToolCall { name, input });
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
