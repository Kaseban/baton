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
        let content = match raw.strip_prefix("---") {
            Some(rest) => match rest.find("\n---") {
                Some(end) => &rest[end + 4..],
                None => raw.as_str(),
            },
            None => raw.as_str(),
        };

        // Two dialects share the #### marker:
        //   - baton's own output: `#### USER` / `#### ASSISTANT` role headers
        //   - aider's native history: `#### <the user's prompt>` with the assistant
        //     reply as unprefixed text below (files start "# aider chat started at ...")
        let role_headers = content.lines().any(|l| {
            matches!(l.trim(), "#### USER" | "#### ASSISTANT" | "#### SYSTEM")
        });

        let mut push = |role: Role, buffer: &mut String| {
            if !buffer.trim().is_empty() {
                messages.push(Message {
                    role,
                    parts: vec![Part::Text {
                        text: buffer.trim().to_string(),
                    }],
                    time_created: ts,
                    origin: Some(Agent::Aider),
                });
                ts += 1;
            }
            buffer.clear();
        };

        if role_headers {
            let mut current_role: Option<Role> = None;
            let mut buffer = String::new();
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(header) = trimmed.strip_prefix("#### ") {
                    if let Some(r) = current_role {
                        push(r, &mut buffer);
                    }
                    current_role = Some(match header.trim() {
                        "ASSISTANT" | "assistant" => Role::Assistant,
                        "SYSTEM" | "system" => Role::System,
                        _ => Role::User,
                    });
                } else {
                    buffer.push_str(line);
                    buffer.push('\n');
                }
            }
            if let Some(r) = current_role {
                push(r, &mut buffer);
            }
        } else {
            let mut user_buf = String::new();
            let mut asst_buf = String::new();
            for line in content.lines() {
                if let Some(prompt) = line.trim().strip_prefix("#### ") {
                    push(Role::Assistant, &mut asst_buf);
                    user_buf.push_str(prompt);
                    user_buf.push('\n');
                } else if line.starts_with("# aider chat started at") {
                    push(Role::User, &mut user_buf);
                    push(Role::Assistant, &mut asst_buf);
                } else {
                    push(Role::User, &mut user_buf);
                    asst_buf.push_str(line);
                    asst_buf.push('\n');
                }
            }
            push(Role::User, &mut user_buf);
            push(Role::Assistant, &mut asst_buf);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Format as _, Role};

    fn read_str(name: &str, content: &str) -> Session {
        // unique dir per test — tests run in parallel
        let dir = std::env::temp_dir().join(format!("baton-aider-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".aider.chat.history.md");
        std::fs::write(&path, content).unwrap();
        let s = Aider::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        s
    }

    #[test]
    fn parses_native_aider_history() {
        let s = read_str(
            "native",
            "# aider chat started at 2024-01-01 12:00:00\n\n#### add a login page\n\nSure, adding login page now.\n\n#### make it blue\n\nDone, it is blue.\n",
        );
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[0].role, Role::User);
        assert_eq!(s.messages[0].parts[0].as_text(), Some("add a login page"));
        assert_eq!(s.messages[1].role, Role::Assistant);
        assert_eq!(s.messages[2].role, Role::User);
        assert_eq!(s.messages[2].parts[0].as_text(), Some("make it blue"));
        assert_eq!(s.messages[3].role, Role::Assistant);
    }

    #[test]
    fn parses_baton_role_headers() {
        let s = read_str("headers", "#### USER\n\nhello\n\n#### ASSISTANT\n\nhi there\n");
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, Role::User);
        assert_eq!(s.messages[0].parts[0].as_text(), Some("hello"));
        assert_eq!(s.messages[1].role, Role::Assistant);
        assert_eq!(s.messages[1].parts[0].as_text(), Some("hi there"));
    }
}
