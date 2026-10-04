//! OpenAI Codex session format: the Codex CLI (`codex`, `codex exec`) and the
//! Codex Desktop app, which write the same rollouts.
//!
//! A rollout is `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<thread>.jsonl`;
//! archiving a thread moves it to `~/.codex/archived_sessions/`. Every line is
//! an envelope `{"timestamp", "ordinal", "type", "payload"}`:
//!   - `session_meta`: thread id, `cwd`, `originator` (`codex-tui`,
//!     `codex_exec`, `Codex Desktop`), `source` (`cli`, `exec`, `vscode`, or
//!     `{"subagent": {"thread_spawn": {"parent_thread_id", "agent_path"}}}`).
//!   - `turn_context`: one per turn, with the turn's `model`.
//!   - `response_item`: what the model was sent and what it said: `message`
//!     (user, assistant, developer), `reasoning`, `function_call` and
//!     `custom_tool_call` with their `_output`s, `agent_message` between agents.
//!   - `event_msg`: app events. `item_completed` records what a tool really
//!     did: a `CommandExecution` with its output and exit code, a `FileChange`
//!     with every file's content or diff, an `McpToolCall`, a web search.
//!   - `compacted`: the context was compacted, and what of it was kept.
//!
//! A thread can span several files: `rollout-<ts>-<thread>_<window>.jsonl`
//! continues `rollout-<ts>-<thread>.jsonl`, and reading any of them reads the
//! whole thread. A subagent's rollout begins with a copy of its parent's
//! history up to `subagent_history_start_ordinal`, which is dropped because
//! the parent is a session of its own; a fork of a user's thread refers to the
//! history it was forked from (`history_base`), which is read from there. The
//! thread's name comes from `session_index.jsonl` in the Codex home.
//!
//! Very old rollouts emit bare ResponseItem lines; the reader accepts both.
//! The writer emits the modern envelope form.

mod items;
mod reader;
mod rollout;
mod script;

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::json;

use crate::canonical::{Agent, Format, Part, Role, Session};

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
        let thread = rollout::load(path)?;
        let rollout::Located { meta, home, .. } = &thread.located;
        let subagent_of = meta.parent_thread_id.as_deref().filter(|_| meta.source.as_deref() == Some("subagent"));
        let provider = meta.model_provider.clone().unwrap_or_else(|| "openai".into());
        let mut reader = reader::Reader::new(provider, meta.timestamp);
        for record in &thread.records {
            reader.record(record);
        }
        let (messages, children) = reader.finish();

        let subagent_title = meta.agent_path.as_deref().map(|path| {
            let task = path.rsplit('/').next().unwrap_or(path);
            match meta.agent_nickname.as_deref() {
                Some(nick) => format!("{task} ({nick})"),
                None => task.to_string(),
            }
        });
        let first_prompt = messages
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| m.parts.iter().filter_map(Part::as_text))
            .find(|t| !t.starts_with("Message Type:"))
            .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(|l| l.chars().take(100).collect());
        let title = home
            .thread_name(&meta.id)
            .or(subagent_title)
            .or(first_prompt)
            .unwrap_or_else(|| "Codex session".to_string());
        let time_created = messages.first().map(|m| m.time_created).unwrap_or(meta.timestamp);
        let time_updated = messages.iter().map(|m| m.time_created).max().unwrap_or(time_created);
        Ok(Session {
            source_id: meta.id.clone(),
            origin: Agent::Codex,
            title,
            time_created,
            time_updated,
            directory: meta.cwd.clone(),
            title_prefix: None,
            parent: subagent_of.filter(|p| home.has_thread(p)).map(str::to_string),
            children,
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
                    Part::Reasoning { text, .. } => {
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

/// What `baton info --from codex` reports about the thread `path` belongs
/// to, read from its `session_meta` alone: enough to decide whether to import
/// it (interactive or automation, user thread or subagent) without converting.
pub fn info(path: &Path) -> anyhow::Result<serde_json::Value> {
    let rollout::Located { meta, home, files, archived } = rollout::locate(path)?;
    let primary = files.first().is_some_and(|f| {
        std::fs::canonicalize(f).ok() == std::fs::canonicalize(path).ok()
    });
    Ok(json!({
        "id": meta.id,
        "primary": primary,
        "files": files,
        "archived": archived,
        "originator": meta.originator,
        "source": meta.source,
        "subagent": meta.subagent,
        "thread_source": meta.thread_source,
        "cli_version": meta.cli_version,
        "model_provider": meta.model_provider,
        "cwd": meta.cwd,
        "parent_thread_id": meta.parent_thread_id,
        "parent_present": meta.parent_thread_id.as_deref().is_some_and(|p| home.has_thread(p)),
        "agent_path": meta.agent_path,
        "agent_nickname": meta.agent_nickname,
        "agent_role": meta.agent_role,
        "forked_from_id": meta.forked_from_id,
        "history_base": meta.history_base.as_ref().map(|(id, _)| id),
        "thread_name": home.thread_name(&meta.id),
    }))
}

#[cfg(test)]
mod fixture_tests;

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
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages: vec![
                Message {
                    role: Role::User,
                    parts: vec![Part::text("run ls for me")],
                    time_created: 0,
                    origin: None,
                    model: None,
                    summary: false,
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Reasoning { text: "planning".into(), signature: None },
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
                    model: None,
                    summary: false,
                },
            ],
        };
        Codex::write(&session, &path).unwrap();
        let back = Codex::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.messages[0].role, Role::User);
        assert_eq!(back.messages[0].parts[0].as_text(), Some("run ls for me"));
        let all_parts: Vec<&Part> = back.messages.iter().flat_map(|m| m.parts.iter()).collect();
        assert!(all_parts.iter().any(|p| matches!(p, Part::Reasoning { text, .. } if text == "planning")));
        assert!(all_parts.iter().any(|p| matches!(p, Part::ToolCall { name, id, input }
            if name == "exec_command" && id.as_deref() == Some("call_7") && input.as_ref().unwrap()["command"] == "ls")),
            "codex's `shell` reads back as the command it ran");
        assert!(all_parts.iter().any(|p| matches!(p, Part::ToolResult { id, output, .. }
            if id.as_deref() == Some("call_7") && output.as_deref() == Some("file.txt"))));
    }
}
