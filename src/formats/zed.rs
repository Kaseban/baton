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
                                if let Some(res) = msg.tool_results.get(&tool.id) {
                                    let output = res
                                        .get("content")
                                        .map(|v| match v {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        });
                                    parts.push(Part::ToolResult {
                                        name: tool.name.clone(),
                                        id: Some(tool.id.clone()),
                                        output,
                                        is_error: res.get("is_error").and_then(|v| v.as_bool()),
                                    });
                                }
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

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        let mut items: Vec<serde_json::Value> = Vec::new();
        for msg in &session.messages {
            match msg.role {
                // Zed has no system role; fold system text into a user message.
                Role::User | Role::System => {
                    // Claude-style transcripts carry tool results in user messages;
                    // Zed keeps them in the preceding agent message's tool_results.
                    for p in &msg.parts {
                        if let Part::ToolResult { name, id, output, is_error } = p {
                            let key = id.clone().unwrap_or_else(|| name.clone());
                            if let Some(results) = items
                                .iter_mut()
                                .rev()
                                .find_map(|i| i.get_mut("Agent"))
                                .and_then(|a| a.get_mut("tool_results"))
                                .and_then(|t| t.as_object_mut())
                            {
                                results.insert(
                                    key,
                                    serde_json::json!({
                                        "content": output.clone().unwrap_or_default(),
                                        "is_error": is_error.unwrap_or(false),
                                    }),
                                );
                            }
                        }
                    }
                    let content: Vec<serde_json::Value> = msg
                        .parts
                        .iter()
                        .filter_map(|p| p.as_text())
                        .map(|t| serde_json::json!({ "Text": { "text": t } }))
                        .collect();
                    if content.is_empty() {
                        continue;
                    }
                    items.push(serde_json::json!({ "User": { "content": content } }));
                }
                Role::Assistant => {
                    let mut content: Vec<serde_json::Value> = Vec::new();
                    let mut tool_results = serde_json::Map::new();
                    for p in &msg.parts {
                        match p {
                            Part::Text { text } => {
                                content.push(serde_json::json!({ "Text": { "text": text } }));
                            }
                            Part::Reasoning { text } => {
                                content.push(serde_json::json!({
                                    "Thinking": { "text": text, "signature": null }
                                }));
                            }
                            Part::ToolCall { name, id, input } => {
                                let id = id.clone().unwrap_or_else(|| name.clone());
                                content.push(serde_json::json!({
                                    "ToolUse": {
                                        "id": id,
                                        "name": name,
                                        "input": input.clone().unwrap_or(serde_json::Value::Object(Default::default())),
                                    }
                                }));
                            }
                            Part::ToolResult { name, id, output, is_error } => {
                                let key = id.clone().unwrap_or_else(|| name.clone());
                                tool_results.insert(
                                    key,
                                    serde_json::json!({
                                        "content": output.clone().unwrap_or_default(),
                                        "is_error": is_error.unwrap_or(false),
                                    }),
                                );
                            }
                            Part::Attachment { .. } => {
                                content.push(serde_json::json!({ "Text": { "text": "[attachment]" } }));
                            }
                        }
                    }
                    if content.is_empty() && tool_results.is_empty() {
                        continue;
                    }
                    items.push(serde_json::json!({
                        "Agent": { "content": content, "tool_results": tool_results }
                    }));
                }
            }
        }
        let out = serde_json::to_string_pretty(&items)?;
        std::fs::write(out_path, out)
            .with_context(|| format!("writing zed conversation {}", out_path.display()))?;
        Ok(())
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
    /// Tool outputs keyed by tool-use id.
    #[serde(default)]
    tool_results: std::collections::HashMap<String, serde_json::Value>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Message, Session};

    #[test]
    fn write_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("baton-zed-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.json");

        let session = Session {
            source_id: "rt".into(),
            origin: Agent::ClaudeCode,
            title: "t".into(),
            time_created: 0,
            time_updated: 0,
            directory: None,
            messages: vec![
                Message {
                    role: Role::User,
                    parts: vec![Part::text("hello zed")],
                    time_created: 0,
                    origin: None,
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Reasoning { text: "hmm".into() },
                        Part::text("hi"),
                        Part::ToolCall {
                            name: "read_file".into(),
                            id: Some("tu_1".into()),
                            input: Some(serde_json::json!({"path": "/x"})),
                        },
                        Part::ToolResult {
                            name: "read_file".into(),
                            id: Some("tu_1".into()),
                            output: Some("contents".into()),
                            is_error: Some(false),
                        },
                    ],
                    time_created: 1,
                    origin: None,
                },
            ],
        };
        Zed::write(&session, &path).unwrap();
        let back = Zed::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.messages[0].role, Role::User);
        assert_eq!(back.messages[0].parts[0].as_text(), Some("hello zed"));
        let asst = &back.messages[1];
        assert!(asst.parts.iter().any(|p| matches!(p, Part::Reasoning { text } if text == "hmm")));
        assert!(asst.parts.iter().any(|p| matches!(p, Part::ToolCall { name, id, .. }
            if name == "read_file" && id.as_deref() == Some("tu_1"))));
        assert!(asst.parts.iter().any(|p| matches!(p, Part::ToolResult { output, .. }
            if output.as_deref() == Some("contents"))));
    }
}
