//! Cursor AI session format.
//!
//! Cursor stores chat history in `~/.cursor/state.vscdb` (SQLite) under the
//! `ItemTable` key `aiService:chats`. The value is a JSON blob of shape:
//!   { "chats": { "<id>": { "title": "...", "messages": [...] } } }
//!
//! Each message has shape:
//!   { "role": "user"|"assistant", "text": "...", "codeBlocks": [...] }
//!
//! Reading requires SQLite — this reader supports both:
//!   1. A direct path to a `.vscdb` file (requires `sqlite` feature)
//!   2. A path to a pre-exported JSON file with the chats shape above
//!
//! For now, only the JSON-export path is implemented. To export from Cursor:
//!   sqlite3 ~/.cursor/state.vscdb "SELECT value FROM ItemTable WHERE key='aiService:chats'" > cursor-chats.json

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Cursor;

impl Format for Cursor {
    const AGENT: Agent = Agent::Cursor;
    const NAME: &'static str = "Cursor";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".cursor")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        // Try JSON shape first (exported chats)
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading cursor session {}", path.display()))?;

        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let now = chrono::Utc::now().timestamp_millis();

        // Try the wrapped shape: { "chats": { "<id>": { "messages": [...] } } }
        // serde_json's preserve_order feature keeps file order, so "first chat"
        // is deterministic. Extra chats are skipped (reported below).
        let chats: CursorChats = serde_json::from_str(&raw).or_else(|_| {
            // Fall back to a bare array of messages
            let msgs: Vec<CursorMessage> = serde_json::from_str(&raw)
                .context("parsing cursor session as CursorChats or bare array")?;
            Ok::<_, anyhow::Error>(CursorChats {
                chats: [(
                    session_id.clone(),
                    serde_json::json!({ "title": "", "messages": msgs }),
                )]
                .into_iter()
                .collect(),
            })
        })?;

        let total = chats.chats.len();
        let (id, chat_value) = chats
            .chats
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no chats found in cursor session file"))?;
        let chat: CursorChat = serde_json::from_value(chat_value)
            .with_context(|| format!("parsing cursor chat {id}"))?;
        if total > 1 {
            eprintln!(
                "cursor export contains {total} chats; converting the first ({id}). Re-run with a single-chat export for the others."
            );
        }

        let mut messages = Vec::new();
        let mut ts = now;
        for msg in chat.messages {
            let role = match msg.role.as_str() {
                "assistant" => Role::Assistant,
                "user" => Role::User,
                _ => continue,
            };
            let text = msg.text.unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            messages.push(Message {
                role,
                parts: vec![Part::Text { text }],
                time_created: ts,
                origin: Some(Agent::Cursor),
            });
            ts += 1;
        }

        let title = if chat.title.is_empty() {
            messages
                .iter()
                .find(|m| m.role == Role::User)
                .and_then(|m| {
                    m.parts.iter().find_map(|p| match p {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                })
                .unwrap_or("Cursor chat")
                .chars()
                .take(60)
                .collect::<String>()
        } else {
            chat.title
        };

        Ok(Session {
            source_id: id,
            origin: Agent::Cursor,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cursor write not implemented. Cursor has no JSON import path.")
    }
}

// serde_json::Map preserves insertion order (preserve_order feature), making
// "first chat in the file" deterministic — a HashMap would pick one at random.
#[derive(Debug, Deserialize)]
struct CursorChats {
    #[serde(default)]
    chats: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CursorChat {
    #[serde(default)]
    title: String,
    #[serde(default)]
    messages: Vec<CursorMessage>,
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct CursorMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    text: Option<String>,
}
