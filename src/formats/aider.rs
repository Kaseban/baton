//! Aider session format.
//!
//! Aider stores chat history per-repo in `.aider.chat.history.md`. Each turn is delimited
//! by markdown headers:
//!
//! ```markdown
//! #### USER
//! <user message text>
//!
//! #### ASSISTANT
//! <assistant message text>
//!
//! #### ASSISTANT
//! ...
//! ```
//!
//! Tool calls appear as fenced code blocks or `<action>` tags within assistant turns.
//! Aider also supports `.aider.chat.history.md` with a YAML front-matter section
//! describing the repo, files, and commit history at the start.
//!
//! There is no separate JSON log by default; the markdown IS the source of truth.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Aider;

impl Format for Aider {
    const AGENT: Agent = Agent::Aider;
    const NAME: &'static str = "Aider";

    fn session_dir() -> PathBuf {
        // Aider is per-repo — look in cwd by default.
        PathBuf::from(".")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading aider history {}", path.display()))?;

        let session_id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("aider")
            .to_string();

        let now = chrono::Utc::now().timestamp_millis();
        let mut messages = Vec::new();
        let mut ts = now;

        // Strip YAML front-matter if present
        let content = if raw.starts_with("---") {
            if let Some(end) = raw[3..].find("\n---") {
                &raw[3 + end + 4..]
            } else {
                raw.as_str()
            }
        } else {
            raw.as_str()
        };

        let mut current_role: Option<Role> = None;
        let mut current_parts: Vec<Part> = Vec::new();
        let mut buffer = String::new();

        let flush = |role: &Option<Role>,
                     parts: &mut Vec<Part>,
                     buffer: &mut String,
                     messages: &mut Vec<Message>,
                     ts: &mut i64| {
            if let Some(r) = role {
                if !buffer.trim().is_empty() {
                    parts.push(Part::Text {
                        text: buffer.trim().to_string(),
                    });
                }
                if !parts.is_empty() {
                    messages.push(Message {
                        role: *r,
                        parts: std::mem::take(parts),
                        time_created: *ts,
                        origin: Some(Agent::Aider),
                    });
                    *ts += 1;
                }
            }
        };

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("#### ") {
                // Flush previous message
                flush(&current_role, &mut current_parts, &mut buffer, &mut messages, &mut ts);

                let header = trimmed.trim_start_matches("#### ").trim();
                current_role = Some(match header {
                    "ASSISTANT" | "assistant" => Role::Assistant,
                    _ => Role::User,
                });
                current_parts.clear();
                buffer.clear();
            } else {
                buffer.push_str(line);
                buffer.push('\n');
            }
        }
        // Flush last message
        flush(&current_role, &mut current_parts, &mut buffer, &mut messages, &mut ts);

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
            "Aider session".to_string()
        } else {
            first_user.chars().take(60).collect()
        };

        Ok(Session {
            source_id: session_id,
            origin: Agent::Aider,
            title,
            time_created: now,
            time_updated: now,
            directory: None,
            messages,
        })
    }

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        for msg in &session.messages {
            let header = match msg.role {
                Role::User => "#### USER",
                Role::Assistant => "#### ASSISTANT",
                Role::System => "#### SYSTEM",
            };
            writeln!(file, "{}\n", header)?;
            for part in &msg.parts {
                if let Some(text) = part.as_text() {
                    writeln!(file, "{}\n", text)?;
                }
            }
        }
        Ok(())
    }
}
