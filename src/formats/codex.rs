//! OpenAI Codex CLI session format.
//!
//! Codex stores sessions as JSONL rollout files under
//! `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`.
//!
//! Modern codex (0.4x+) wraps every line in an envelope:
//!   `{"timestamp":"...","type":"session_meta"|"response_item"|"event_msg"|..., "payload":{...}}`
//! where a `response_item` payload is a ResponseItem (tagged by `type`):
//!   - `{"type":"message","role":"user"|"assistant","content":[{type:"input_text"|"output_text",text}]}`
//!   - `{"type":"reasoning","summary":[{type:"summary_text",text}],"content":[{type:"reasoning_text",text}]}`
//!   - `{"type":"function_call","name":"...","arguments":"...","call_id":"..."}`
//!   - `{"type":"function_call_output","call_id":"...","output":"..." | {"content":"...","success":bool}}`
//!
//! Very old rollouts (and other tools) emit bare ResponseItem lines; the reader
//! accepts both. The writer emits the modern envelope form.

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
        let mut session_id = session_id;
        let mut directory: Option<String> = None;

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Modern envelope line, or bare legacy ResponseItem.
            let mut line_ts: Option<i64> = None;
            let item: ResponseItem = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) if v.get("payload").is_some() => {
                    line_ts = v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|dt| dt.timestamp_millis());
                    let env_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let payload = v.get("payload").cloned().unwrap_or_default();
                    match env_type {
                        "session_meta" => {
                            if let Some(id) = payload.get("id").and_then(|i| i.as_str()) {
                                session_id = id.to_string();
                            }
                            directory = payload
                                .get("cwd")
                                .and_then(|c| c.as_str())
                                .map(|s| s.to_string());
                            continue;
                        }
                        "response_item" => match serde_json::from_value(payload) {
                            Ok(item) => item,
                            Err(_) => continue,
                        },
                        _ => continue,
                    }
                }
                Ok(v) => match serde_json::from_value(v) {
                    Ok(item) => item,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let line_ts = line_ts.unwrap_or(now);

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
                ResponseItem::Reasoning { text, summary, content } => {
                    // Modern codex splits reasoning across summary/content arrays;
                    // legacy (and baton ≤0.1) used a flat `text` field.
                    let mut combined: Vec<String> = Vec::new();
                    if !text.is_empty() {
                        combined.push(text.clone());
                    }
                    combined.extend(
                        content
                            .iter()
                            .chain(summary.iter())
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .filter(|t| !t.is_empty())
                            .map(|t| t.to_string()),
                    );
                    if combined.is_empty() {
                        continue;
                    }
                    (
                        Role::Assistant,
                        vec![Part::Reasoning { text: combined.join("\n") }],
                    )
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
                    // Output is a plain string in old rollouts, an object
                    // {"content": "...", "success": bool} in new ones.
                    let (text, is_error) = match output {
                        Some(serde_json::Value::String(s)) => (s.clone(), None),
                        Some(obj) => (
                            obj.get("content")
                                .and_then(|c| c.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| obj.to_string()),
                            obj.get("success").and_then(|s| s.as_bool()).map(|s| !s),
                        ),
                        None => (String::new(), None),
                    };
                    (
                        Role::Assistant,
                        vec![Part::ToolResult {
                            name: "function".into(),
                            id: call_id.clone(),
                            output: Some(text),
                            is_error,
                        }],
                    )
                }
                _ => continue,
            };

            if parts.is_empty() {
                continue;
            }

            if first_ts.is_none() {
                first_ts = Some(line_ts);
            }
            last_ts = line_ts.max(last_ts);
            messages.push(Message {
                role,
                parts,
                time_created: line_ts,
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
            directory,
            messages,
        })
    }

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;

        // codex resume requires a UUID session id in session_meta.
        let meta_id = uuid::Uuid::parse_str(&session.source_id)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
        let meta_ts = iso(session.time_created);
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "timestamp": meta_ts,
                "type": "session_meta",
                "payload": {
                    "id": meta_id,
                    "timestamp": meta_ts,
                    "cwd": session.directory.clone().unwrap_or_else(|| ".".to_string()),
                    "originator": "baton",
                    "cli_version": env!("CARGO_PKG_VERSION"),
                    "instructions": null,
                },
            })
        )?;

        for msg in &session.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            let ts = iso(msg.time_created);
            let mut emit = |payload: serde_json::Value| -> anyhow::Result<()> {
                writeln!(
                    file,
                    "{}",
                    serde_json::json!({
                        "timestamp": ts,
                        "type": "response_item",
                        "payload": payload,
                    })
                )?;
                Ok(())
            };
            // One ResponseItem line per part, mirroring how codex interleaves
            // text / reasoning / function_call records in a rollout.
            let mut texts: Vec<serde_json::Value> = Vec::new();
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
                        flush_message(&mut texts, role, &mut emit)?;
                        emit(serde_json::json!({
                            "type": "reasoning",
                            "summary": [{ "type": "summary_text", "text": text }],
                            "content": [],
                        }))?;
                    }
                    Part::ToolCall { name, id, input } => {
                        flush_message(&mut texts, role, &mut emit)?;
                        let arguments = input
                            .as_ref()
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".to_string());
                        emit(serde_json::json!({
                            "type": "function_call",
                            "name": name,
                            "arguments": arguments,
                            "call_id": id,
                        }))?;
                    }
                    Part::ToolResult { id, output, is_error, .. } => {
                        flush_message(&mut texts, role, &mut emit)?;
                        emit(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": id,
                            "output": {
                                "content": output.clone().unwrap_or_default(),
                                "success": !is_error.unwrap_or(false),
                            },
                        }))?;
                    }
                    Part::Attachment { .. } => {
                        texts.push(serde_json::json!({
                            "type": if msg.role == Role::Assistant { "output_text" } else { "input_text" },
                            "text": "[attachment]",
                        }));
                    }
                }
            }
            flush_message(&mut texts, role, &mut emit)?;
        }
        Ok(())
    }
}

/// Emit accumulated text content as a single `message` ResponseItem.
fn flush_message(
    texts: &mut Vec<serde_json::Value>,
    role: &str,
    emit: &mut impl FnMut(serde_json::Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if !texts.is_empty() {
        emit(serde_json::json!({
            "type": "message",
            "role": role,
            "content": std::mem::take(texts),
        }))?;
    }
    Ok(())
}

fn iso(ts_millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_millis)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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
        #[serde(default)]
        summary: Vec<serde_json::Value>,
        #[serde(default)]
        content: Vec<serde_json::Value>,
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
        output: Option<serde_json::Value>,
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
