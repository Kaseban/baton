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
                    let r = match role.as_str() {
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        _ => Role::User,
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

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        for msg in &session.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            // One ResponseItem line per part, mirroring how codex interleaves
            // text / reasoning / function_call records in a rollout.
            let mut texts: Vec<serde_json::Value> = Vec::new();
            let flush_texts = |texts: &mut Vec<serde_json::Value>,
                                   file: &mut std::fs::File|
             -> anyhow::Result<()> {
                if !texts.is_empty() {
                    let line = serde_json::json!({
                        "type": "message",
                        "role": role,
                        "content": std::mem::take(texts),
                    });
                    writeln!(file, "{}", line)?;
                }
                Ok(())
            };
            for part in &msg.parts {
                match part {
                    Part::Text { text } => {
                        let content_type = if msg.role == Role::Assistant {
                            "output_text"
                        } else {
                            "input_text"
                        };
                        texts.push(serde_json::json!({ "type": content_type, "text": text }));
                    }
                    Part::Reasoning { text } => {
                        flush_texts(&mut texts, &mut file)?;
                        writeln!(
                            file,
                            "{}",
                            serde_json::json!({ "type": "reasoning", "text": text })
                        )?;
                    }
                    Part::ToolCall { name, id, input } => {
                        flush_texts(&mut texts, &mut file)?;
                        let arguments = input
                            .as_ref()
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".to_string());
                        writeln!(
                            file,
                            "{}",
                            serde_json::json!({
                                "type": "function_call",
                                "name": name,
                                "arguments": arguments,
                                "call_id": id,
                            })
                        )?;
                    }
                    Part::ToolResult { id, output, .. } => {
                        flush_texts(&mut texts, &mut file)?;
                        writeln!(
                            file,
                            "{}",
                            serde_json::json!({
                                "type": "function_call_output",
                                "output": output.clone().unwrap_or_default(),
                                "call_id": id,
                            })
                        )?;
                    }
                    Part::Attachment { .. } => {
                        texts.push(serde_json::json!({
                            "type": if msg.role == Role::Assistant { "output_text" } else { "input_text" },
                            "text": "[attachment]",
                        }));
                    }
                }
            }
            flush_texts(&mut texts, &mut file)?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Message, Session};

    #[test]
    fn write_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("baton-codex-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.jsonl");

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
                    parts: vec![Part::text("run ls for me")],
                    time_created: 0,
                    origin: None,
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Reasoning { text: "planning".into() },
                        Part::text("running it"),
                        Part::ToolCall {
                            name: "shell".into(),
                            id: Some("call_7".into()),
                            input: Some(serde_json::json!({"command": ["ls"]})),
                        },
                        Part::ToolResult {
                            name: "shell".into(),
                            id: Some("call_7".into()),
                            output: Some("file.txt".into()),
                            is_error: None,
                        },
                    ],
                    time_created: 1,
                    origin: None,
                },
            ],
        };
        Codex::write(&session, &path).unwrap();
        let back = Codex::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.messages[0].role, Role::User);
        assert_eq!(back.messages[0].parts[0].as_text(), Some("run ls for me"));
        let all_parts: Vec<&Part> = back.messages.iter().flat_map(|m| m.parts.iter()).collect();
        assert!(all_parts.iter().any(|p| matches!(p, Part::Reasoning { text } if text == "planning")));
        assert!(all_parts.iter().any(|p| matches!(p, Part::ToolCall { name, id, input }
            if name == "shell" && id.as_deref() == Some("call_7") && input.as_ref().unwrap()["command"][0] == "ls")));
        assert!(all_parts.iter().any(|p| matches!(p, Part::ToolResult { id, output, .. }
            if id.as_deref() == Some("call_7") && output.as_deref() == Some("file.txt"))));
    }
}
