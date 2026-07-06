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
            if let Ok(entry) = serde_json::from_str::<GeminiEntry>(&raw) {
                vec![entry]
            } else if let Ok(wrapper) = serde_json::from_str::<GeminiWrapper>(&raw) {
                wrapper
                    .candidates
                    .into_iter()
                    .map(|c| GeminiEntry {
                        role: c.content.role,
                        parts: c.content.parts,
                    })
                    .collect()
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
                    GeminiPart::Text { text } => {
                        if text.is_empty() {
                            None
                        } else {
                            Some(Part::Text { text: text.clone() })
                        }
                    }
                    GeminiPart::Thought { text, .. } => {
                        if text.is_empty() {
                            None
                        } else {
                            Some(Part::Reasoning { text: text.clone() })
                        }
                    }
                    GeminiPart::FunctionCall { function_call } => Some(Part::ToolCall {
                        name: function_call.name.clone(),
                        input: Some(function_call.args.clone()),
                    }),
                    GeminiPart::FunctionResponse { function_response } => {
                        let output = serde_json::to_string(&function_response.response).ok();
                        Some(Part::ToolResult {
                            name: function_response.name.clone(),
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

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("gemini-cli write not implemented yet.")
    }
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum GeminiPart {
    Text { text: String },
    Thought { text: String, #[allow(dead_code)] thought: Option<bool> },
    FunctionCall { function_call: GeminiFunctionCall },
    FunctionResponse { function_response: GeminiFunctionResponse },
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
