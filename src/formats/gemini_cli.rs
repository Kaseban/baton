//! Gemini CLI session format.
//!
//! Gemini CLI stores sessions under `~/.gemini/tmp/<session-id>/` as chunked transcript
//! JSON files. The transcript schema mirrors Google's `GenerateContentResponse`:
//!   {
//!     "candidates": [{
//!       "content": {
//!         "role": "user"|"model",
//!         "parts": [
//!           { "text": "..." },
//!           { "functionCall": { "name": "...", "args": {...} } },
//!           { "functionResponse": { "name": "...", "response": {...} } },
//!           { "thought": true, "text": "..." }
//!         ]
//!       }
//!     }]
//!   }
//!
//! We also accept the simpler "parts at root" shape: `{ "role": "...", "parts": [...] }`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct GeminiCli;

impl Format for GeminiCli {
    const AGENT: Agent = Agent::GeminiCli;
    const NAME: &'static str = "Gemini CLI";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".gemini")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading gemini session {}", path.display()))?;

        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let now = chrono::Utc::now().timestamp_millis();

        // Try candidate-based shape first, then bare content shape
        let entries: Vec<GeminiEntry> = if raw.trim().starts_with('[') {
            serde_json::from_str::<Vec<GeminiEntry>>(&raw)?
        } else if raw.trim().starts_with('{') {
            // Could be a single entry or a wrapper with candidates
            // Specific shapes first: GeminiEntry has all-default fields, so it
            // would false-match any object (yielding an empty session).
            if let Ok(chat) = serde_json::from_str::<GeminiChatFile>(&raw) {
                // Session recording shape: ~/.gemini/tmp/<hash>/chats/session-*.json
                chat.messages
                    .into_iter()
                    .map(|m| {
                        let mut parts = Vec::new();
                        for t in m.thoughts {
                            let text = [t.subject, t.description]
                                .into_iter()
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(": ");
                            if !text.is_empty() {
                                parts.push(GeminiPart::Text { text, thought: Some(true) });
                            }
                        }
                        if !m.content.is_empty() {
                            parts.push(GeminiPart::Text { text: m.content, thought: None });
                        }
                        GeminiEntry {
                            role: if m.msg_type == "user" { "user".into() } else { "model".into() },
                            parts,
                        }
                    })
                    .collect()
            } else if let Ok(wrapper) = serde_json::from_str::<GeminiWrapper>(&raw) {
                wrapper
                    .candidates
                    .into_iter()
                    .map(|c| GeminiEntry {
                        role: c.content.role,
                        parts: c.content.parts,
                    })
                    .collect()
            } else if let Ok(entry) = serde_json::from_str::<GeminiEntry>(&raw) {
                vec![entry]
            } else {
                // Try NDJSON (one entry per line)
                raw.lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str::<GeminiEntry>(l).ok())
                    .collect()
            }
        } else {
            Vec::new()
        };

        let mut messages = Vec::new();
        let mut ts = now;

        for entry in entries {
            let role = match entry.role.as_str() {
                "model" | "assistant" => Role::Assistant,
                "user" => Role::User,
                _ => continue,
            };
            let parts: Vec<Part> = entry
                .parts
                .iter()
                .filter_map(|p| match p {
                    GeminiPart::Text { text, thought } => {
                        if text.is_empty() {
                            None
                        } else if thought.unwrap_or(false) {
                            Some(Part::Reasoning { text: text.clone() })
                        } else {
                            Some(Part::Text { text: text.clone() })
                        }
                    }
                    GeminiPart::FunctionCall { function_call } => Some(Part::ToolCall {
                        name: function_call.name.clone(),
                        // Gemini pairs call/response by function name, not a call id.
                        id: Some(function_call.name.clone()),
                        input: Some(function_call.args.clone()),
                    }),
                    GeminiPart::FunctionResponse { function_response } => {
                        let output = serde_json::to_string(&function_response.response).ok();
                        Some(Part::ToolResult {
                            name: function_response.name.clone(),
                            id: Some(function_response.name.clone()),
                            output,
                            is_error: None,
                        })
                    }
                })
                .collect();
            if parts.is_empty() {
                continue;
            }
            messages.push(Message {
                role,
                parts,
                time_created: ts,
                origin: Some(Agent::GeminiCli),
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
            "Gemini session".to_string()
        } else {
            first_user.chars().take(60).collect()
        };

        Ok(Session {
            source_id: session_id,
            origin: Agent::GeminiCli,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    /// Write the checkpoint shape (`/chat resume` compatible): a JSON array of
    /// `{"role": "user"|"model", "parts": [...]}` Content objects.
    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for msg in &session.messages {
            let role = match msg.role {
                Role::Assistant => "model",
                // Gemini has no system role in checkpoints; fold into user.
                Role::User | Role::System => "user",
            };
            let mut parts: Vec<serde_json::Value> = Vec::new();
            for p in &msg.parts {
                match p {
                    Part::Text { text } => parts.push(serde_json::json!({ "text": text })),
                    Part::Reasoning { text } => {
                        parts.push(serde_json::json!({ "text": text, "thought": true }))
                    }
                    Part::ToolCall { name, input, .. } => parts.push(serde_json::json!({
                        "functionCall": {
                            "name": name,
                            "args": input.clone().unwrap_or(serde_json::Value::Object(Default::default())),
                        }
                    })),
                    Part::ToolResult { name, output, .. } => parts.push(serde_json::json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "output": output.clone().unwrap_or_default() },
                        }
                    })),
                    Part::Attachment { .. } => {
                        parts.push(serde_json::json!({ "text": "[attachment]" }))
                    }
                }
            }
            if parts.is_empty() {
                continue;
            }
            entries.push(serde_json::json!({ "role": role, "parts": parts }));
        }
        let out = serde_json::to_string_pretty(&entries)?;
        std::fs::write(out_path, out)
            .with_context(|| format!("writing gemini checkpoint {}", out_path.display()))?;
        Ok(())
    }
}

/// Session recording file: `~/.gemini/tmp/<hash>/chats/session-*.json`.
#[derive(Debug, Deserialize)]
struct GeminiChatFile {
    messages: Vec<GeminiChatMessage>,
}

#[derive(Debug, Deserialize)]
struct GeminiChatMessage {
    #[serde(rename = "type", default)]
    msg_type: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    thoughts: Vec<GeminiThought>,
}

#[derive(Debug, Deserialize)]
struct GeminiThought {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct GeminiWrapper {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
}

#[derive(Debug, Deserialize)]
struct GeminiEntry {
    #[serde(default)]
    role: String,
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

// Real Gemini parts are flat objects — `{"text":"..."}`, `{"functionCall":{...}}` —
// so this must be untagged (an externally-tagged enum would expect `{"text":{"text":...}}`).
// Variant order matters: functionCall/functionResponse first, text last.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
    Text {
        text: String,
        /// `{"text":"...","thought":true}` marks a reasoning part.
        #[serde(default)]
        thought: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionResponse {
    name: String,
    #[serde(default)]
    response: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Message, Session};

    #[test]
    fn write_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("baton-gemini-rt-{}", std::process::id()));
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
                    parts: vec![Part::text("hello gemini")],
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
        GeminiCli::write(&session, &path).unwrap();
        let back = GeminiCli::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.messages[0].role, Role::User);
        assert_eq!(back.messages[0].parts[0].as_text(), Some("hello gemini"));
        let asst = &back.messages[1];
        assert!(asst.parts.iter().any(|p| matches!(p, Part::Reasoning { text } if text == "hmm")));
        assert!(asst.parts.iter().any(|p| matches!(p, Part::ToolCall { name, .. } if name == "read_file")));
        assert!(asst.parts.iter().any(|p| matches!(p, Part::ToolResult { name, .. } if name == "read_file")));
    }

    #[test]
    fn reads_chat_recording_shape() {
        let dir = std::env::temp_dir().join(format!("baton-gemini-chat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-1.json");
        std::fs::write(
            &path,
            r#"{"sessionId":"abc","messages":[
                {"type":"user","content":"question"},
                {"type":"gemini","content":"answer","thoughts":[{"subject":"Plan","description":"do thing"}]}
            ]}"#,
        )
        .unwrap();
        let s = GeminiCli::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, Role::User);
        assert_eq!(s.messages[0].parts[0].as_text(), Some("question"));
        let asst = &s.messages[1];
        assert_eq!(asst.role, Role::Assistant);
        assert!(asst.parts.iter().any(|p| matches!(p, Part::Reasoning { text } if text == "Plan: do thing")));
        assert!(asst.parts.iter().any(|p| p.as_text() == Some("answer")));
    }
}
