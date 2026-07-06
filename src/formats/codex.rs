//! OpenAI Codex CLI session format.
//!
//! Codex stores sessions as JSONL rollout files under `~/.codex/sessions/<YYYY-MM-DD>/<thread-id>.jsonl`.
//! Each line is a `ResponseItem` (tagged by `type`):
//!   - `{"type":"message","role":"user"|"assistant","content":[{type:"input_text"|"output_text",text}]}` 
//!   - `{"type":"reasoning","text":"..."}`
//!   - `{"type":"function_call","name":"...","arguments":"..."}`
//!   - `{"type":"function_call_output","output":"..."}`
//!   - `{"type":"file_change_call",...}`, etc.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Codex;

impl Format for Codex {
    const AGENT: Agent = Agent::Codex;
    const NAME: &'static str = "Codex CLI";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex")
            .join("sessions")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading codex session {}", path.display()))?;
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let mut messages = Vec::new();
        let mut first_ts: Option<i64> = None;
        let mut last_ts: i64 = 0;
        let now = chrono::Utc::now().timestamp_millis();

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let item: ResponseItem = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let (role, parts) = match &item {
                ResponseItem::Message { role, content, .. } => {
                    let r = if role == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    };
                    let parts: Vec<Part> = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                                if text.is_empty() {
                                    None
                                } else {
                                    Some(Part::Text { text: text.clone() })
                                }
                            }
                            ContentItem::Other => None,
                        })
                        .collect();
                    (r, parts)
                }
                ResponseItem::Reasoning { text } => {
                    (Role::Assistant, vec![Part::Reasoning { text: text.clone() }])
                }
                ResponseItem::FunctionCall { name, arguments, call_id } => {
                    let input = arguments
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
                    (
                        Role::Assistant,
                        vec![Part::ToolCall {
                            name: name.clone().unwrap_or_default(),
                            id: call_id.clone(),
                            input,
                        }],
                    )
                }
                ResponseItem::FunctionCallOutput { output, call_id } => {
                    let text = output.clone().unwrap_or_default();
                    (
                        Role::Assistant,
                        vec![Part::ToolResult {
                            name: "function".into(),
                            id: call_id.clone(),
                            output: Some(text),
                            is_error: None,
                        }],
                    )
                }
                _ => continue,
            };

            if parts.is_empty() {
                continue;
            }

            if first_ts.is_none() {
                first_ts = Some(now);
            }
            last_ts = now;
            messages.push(Message {
                role,
                parts,
                time_created: now,
                origin: Some(Agent::Codex),
            });
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
        let title = truncate_title(first_user);

        Ok(Session {
            source_id: session_id,
            origin: Agent::Codex,
            title,
            time_created: first_ts.unwrap_or(0),
            time_updated: last_ts,
            directory: None,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("codex write not implemented. Codex resumes via ~/.codex/sessions/ directly.")
    }
}

fn truncate_title(s: &str) -> String {
    let s = s.trim().replace('\n', " ");
    if s.len() > 60 {
        s.chars().take(60).collect()
    } else {
        s.to_string()
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseItem {
    Message {
        #[serde(default)]
        role: String,
        #[serde(default)]
        content: Vec<ContentItem>,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        arguments: Option<String>,
        #[serde(default)]
        call_id: Option<String>,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        call_id: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
    #[serde(other)]
    Other,
}
