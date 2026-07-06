//! Claude Code session format.
//!
//! Claude Code stores sessions as JSONL under `~/.claude/projects/<encoded-path>/<session-id>.jsonl`.
//! Each line is a JSON object with a `type` field. The relevant ones for a transcript:
//!   - `{"type":"user","message":{"role":"user","content":...},"timestamp":"..."}`
//!   - `{"type":"assistant","message":{"role":"assistant","content":[...]},...}`
//!
//! `content` may be a plain string or an array of typed blocks:
//!   - `{"type":"text","text":"..."}`
//!   - `{"type":"thinking","thinking":"..."}`
//!   - `{"type":"tool_use","name":"...","input":{...}}`
//!   - `{"type":"tool_result","content":"..."}`

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::DateTime;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, Part, Role, Session, SessionRef};

pub struct ClaudeCode;

impl Format for ClaudeCode {
    const AGENT: Agent = Agent::ClaudeCode;
    const NAME: &'static str = "Claude Code";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("projects")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading claude session {}", path.display()))?;
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let mut messages = Vec::new();
        let mut first_ts: Option<i64> = None;
        let mut last_ts: i64 = 0;

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: Entry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            match entry.entry_type.as_str() {
                "user" | "assistant" => {
                    let Some(msg) = entry.message else { continue };
                    let role = match msg.role.as_str() {
                        "user" => Role::User,
                        "assistant" => Role::Assistant,
                        _ => continue,
                    };
                    let (parts, has_content) = parse_content(&msg.content);
                    if !has_content {
                        continue;
                    }
                    // Skip Claude's own meta/caveat injected user messages.
                    if role == Role::User
                        && let Some(Part::Text { text }) = parts.first()
                            && is_meta_user(text) {
                                continue;
                            }
                    let ts = parse_ts(entry.timestamp.as_deref(), &msg.timestamp);
                    if first_ts.is_none() {
                        first_ts = Some(ts);
                    }
                    last_ts = ts.max(last_ts);
                    messages.push(Message {
                        role,
                        parts,
                        time_created: ts,
                        origin: Some(Agent::ClaudeCode),
                    });
                }
                _ => continue,
            }
        }

        let mut session = Session {
            source_id: session_id,
            origin: Agent::ClaudeCode,
            title: String::new(),
            time_created: first_ts.unwrap_or(0),
            time_updated: last_ts,
            directory: None,
            messages,
        };
        session.title = truncate_title(session.first_user_text().unwrap_or(""));
        Ok(session)
    }

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        let session_id = &session.source_id;
        let cwd = session.directory.clone().unwrap_or_default();
        let mut parent_uuid: Option<String> = None;
        for msg in &session.messages {
            let role_str = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            let ts = chrono::DateTime::from_timestamp_millis(msg.time_created)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let content: serde_json::Value = if msg.parts.len() == 1 {
                if let Some(Part::Text { text }) = msg.parts.first() {
                    serde_json::Value::String(text.clone())
                } else {
                    parts_to_claude_content(&msg.parts)
                }
            } else {
                parts_to_claude_content(&msg.parts)
            };
            let uuid = uuid::Uuid::new_v4().to_string();
            let entry = serde_json::json!({
                "type": role_str,
                "uuid": uuid,
                "parentUuid": parent_uuid,
                "sessionId": session_id,
                "cwd": cwd,
                "message": {
                    "role": role_str,
                    "content": content,
                    "timestamp": ts,
                },
                "timestamp": ts,
            });
            parent_uuid = Some(uuid);
            writeln!(file, "{}", entry)?;
        }
        Ok(())
    }

    fn list() -> Vec<SessionRef> {
        let dir = Self::session_dir();
        if !dir.exists() {
            return Vec::new();
        }
        let mut out = Vec::new();
        // Claude stores projects as encoded dirs; each contains .jsonl session files.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for proj_dir in rd.flatten() {
                let pd = proj_dir.path();
                if !pd.is_dir() {
                    continue;
                }
                if let Ok(sessions) = std::fs::read_dir(&pd) {
                    for sf in sessions.flatten() {
                        let path = sf.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                            continue;
                        }
                        let id = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let mtime = sf
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        out.push(SessionRef {
                            agent: Agent::ClaudeCode,
                            id,
                            title: String::new(),
                            path,
                            mtime,
                        });
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(rename = "type", default)]
    entry_type: String,
    #[serde(default)]
    message: Option<ClaudeMessage>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: serde_json::Value,
    #[serde(default)]
    timestamp: Option<String>,
}

fn parse_content(content: &serde_json::Value) -> (Vec<Part>, bool) {
    let mut parts = Vec::new();
    let mut has_content = false;
    match content {
        serde_json::Value::String(s) => {
            if !s.is_empty() {
                parts.push(Part::Text { text: s.clone() });
                has_content = true;
            }
        }
        serde_json::Value::Array(arr) => {
            for block in arr {
                if let Some(btype) = block.get("type").and_then(|v| v.as_str()) {
                    match btype {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str())
                                && !text.is_empty() {
                                    parts.push(Part::Text {
                                        text: text.to_string(),
                                    });
                                    has_content = true;
                                }
                        }
                        "thinking" => {
                            if let Some(text) = block.get("thinking").and_then(|v| v.as_str()) {
                                parts.push(Part::Reasoning {
                                    text: text.to_string(),
                                });
                                has_content = true;
                            }
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let input = block.get("input").cloned();
                            parts.push(Part::ToolCall { name, id, input });
                            has_content = true;
                        }
                        "tool_result" => {
                            let id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let output = match block.get("content") {
                                Some(serde_json::Value::String(s)) => Some(s.clone()),
                                Some(other) => Some(other.to_string()),
                                None => None,
                            };
                            let is_error = block.get("is_error").and_then(|v| v.as_bool());
                            parts.push(Part::ToolResult {
                                name: "tool".to_string(),
                                id,
                                output,
                                is_error,
                            });
                            has_content = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
    (parts, has_content)
}

fn is_meta_user(text: &str) -> bool {
    let t = text.trim_start();
    const CAVEATS: &[&str] = &[
        "<local-command",
        "Caveat:",
        "<command-message>",
        "<command-name>",
        "<user-memory>",
        "[Request interrupted",
    ];
    CAVEATS.iter().any(|c| t.starts_with(c))
}

fn parse_ts(entry_ts: Option<&str>, msg_ts: &Option<String>) -> i64 {
    let ts = entry_ts.or(msg_ts.as_deref());
    ts.and_then(|s| {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis())
    })
    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
}

fn truncate_title(s: &str) -> String {
    let s = s.trim().replace('\n', " ");
    if s.len() > 60 {
        s.chars().take(60).collect()
    } else {
        s.to_string()
    }
}

fn parts_to_claude_content(parts: &[Part]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = parts
        .iter()
        .map(|p| match p {
            Part::Text { text } => serde_json::json!({"type":"text","text":text}),
            Part::Reasoning { text } => serde_json::json!({"type":"thinking","thinking":text}),
            Part::ToolCall { name, id, input } => serde_json::json!({
                "type":"tool_use",
                "id": id.clone().unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4().simple())),
                "name": name,
                "input": input.clone().unwrap_or(serde_json::Value::Object(Default::default())),
            }),
            Part::ToolResult {
                name: _,
                id,
                output,
                is_error,
            } => serde_json::json!({
                "type":"tool_result",
                "tool_use_id": id.clone().unwrap_or_default(),
                "content": output.clone().unwrap_or_default(),
                "is_error": is_error.unwrap_or(false),
            }),
            Part::Attachment { mime, path, data } => {
                let mut o = serde_json::json!({"type":"attachment","mime": mime});
                if let Some(p) = path {
                    o["path"] = serde_json::Value::String(p.clone());
                }
                if let Some(d) = data {
                    o["data"] = serde_json::Value::String(d.clone());
                }
                o
            }
        })
        .collect();
    serde_json::Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Role;

    #[test]
    fn read_real_shaped_jsonl() {
        let dir = std::env::temp_dir().join(format!("baton-claude-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fa88b429-0000-0000-0000-000000000000.jsonl");
        let jsonl = concat!(
            r#"{"type":"user","sessionId":"fa88b429","message":{"role":"user","content":"hello"},"timestamp":"2024-01-01T00:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2024-01-01T00:00:01Z"}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file.txt"}]},"timestamp":"2024-01-01T00:00:02Z"}"#, "\n",
            r#"{"type":"summary","summary":"ignored"}"#, "\n",
        );
        std::fs::write(&path, jsonl).unwrap();
        let s = ClaudeCode::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(s.messages.len(), 3);
        assert_eq!(s.title, "hello");
        assert_eq!(s.time_created, 1704067200000);
        let asst = &s.messages[1];
        assert_eq!(asst.role, Role::Assistant);
        assert!(matches!(&asst.parts[0], Part::Reasoning { text } if text == "hmm"));
        assert!(matches!(&asst.parts[2], Part::ToolCall { name, id, .. } if name == "Bash" && id.as_deref() == Some("toolu_1")));
        assert!(matches!(&s.messages[2].parts[0], Part::ToolResult { id, output, .. } if id.as_deref() == Some("toolu_1") && output.as_deref() == Some("file.txt")));
    }

    #[test]
    fn write_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("baton-claude-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("11111111-2222-3333-4444-555555555555.jsonl");

        let session = Session {
            source_id: "11111111-2222-3333-4444-555555555555".into(),
            origin: Agent::Opencode,
            title: "t".into(),
            time_created: 1000,
            time_updated: 2000,
            directory: Some("/tmp".into()),
            messages: vec![
                Message {
                    role: Role::User,
                    parts: vec![Part::text("question")],
                    time_created: 1000,
                    origin: None,
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![
                        Part::text("answer"),
                        Part::ToolCall {
                            name: "Read".into(),
                            id: Some("toolu_9".into()),
                            input: Some(serde_json::json!({"path": "/x"})),
                        },
                    ],
                    time_created: 1001,
                    origin: None,
                },
            ],
        };
        ClaudeCode::write(&session, &path).unwrap();

        // every line carries the uuid/parentUuid/sessionId chain
        let raw = std::fs::read_to_string(&path).unwrap();
        for line in raw.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["sessionId"], "11111111-2222-3333-4444-555555555555");
            assert!(v["uuid"].is_string());
        }

        let back = ClaudeCode::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(back.messages.len(), 2);
        assert!(matches!(&back.messages[1].parts[1], Part::ToolCall { name, id, .. } if name == "Read" && id.as_deref() == Some("toolu_9")));
    }
}
