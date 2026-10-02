//! Canonical session representation.
//!
//! Every agent format is read into [`Session`] and written from [`Session`].
//! This makes adding a new format O(1) instead of O(N×M) per-pair converters.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Originating agent for a session or message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Opencode,
    Codex,
    Cursor,
    Continue,
    Cline,
    Zed,
    Aider,
    GeminiCli,
    Unknown,
}

impl Agent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Opencode => "opencode",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Continue => "continue",
            Agent::Cline => "cline",
            Agent::Zed => "zed",
            Agent::Aider => "aider",
            Agent::GeminiCli => "gemini-cli",
            Agent::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Some(match s.as_str() {
            "claude" | "claude-code" | "claudecode" => Agent::ClaudeCode,
            "opencode" => Agent::Opencode,
            "codex" => Agent::Codex,
            "cursor" => Agent::Cursor,
            "continue" => Agent::Continue,
            "cline" | "roo" | "roo-code" | "roocode" => Agent::Cline,
            "zed" => Agent::Zed,
            "aider" => Agent::Aider,
            "gemini" | "gemini-cli" => Agent::GeminiCli,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single block of a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Part {
    /// Plain text content from user or assistant.
    Text { text: String },
    /// Reasoning / thinking trace (Claude "thinking", o1 chain-of-thought, etc.).
    Reasoning {
        text: String,
        /// Provider-issued signature over the reasoning block (Anthropic
        /// `thinking.signature`).
        ///
        /// Anthropic signs every thinking block it emits and *requires* the
        /// signature to be echoed back verbatim when that block is replayed in a
        /// later request; a client can neither generate nor re-sign one. Dropping
        /// it makes the converted session unusable in the target agent, which
        /// fails with a 400 as soon as the history is replayed. Formats with no
        /// equivalent concept simply leave this `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A tool invocation made by the assistant.
    ToolCall {
        name: String,
        /// Source-format call id (e.g. Anthropic `tool_use.id`), used to pair with the result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// The result of a tool invocation.
    ToolResult {
        name: String,
        /// Call id this result answers (pairs with `ToolCall::id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// A file or image attachment (path or base64).
    Attachment {
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
}

impl Part {
    // Only exercised by tests today; part of the intended public surface.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn text(s: impl Into<String>) -> Self {
        Part::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        if let Part::Text { text } = self {
            Some(text)
        } else {
            None
        }
    }
}

/// A message in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    /// Epoch milliseconds.
    #[serde(default)]
    pub time_created: i64,
    /// Origin agent that produced this message (useful when merging).
    #[serde(default)]
    pub origin: Option<Agent>,
    /// Model that produced this message, when the source format records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
}

/// A model as a provider id plus that provider's model id, e.g. `anthropic` +
/// `claude-opus-4-8`, which is how opencode addresses models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Canonical session — the intermediate representation every format funnels through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// ID in the *source* agent's namespace (e.g. a Claude `sessionId` UUID).
    pub source_id: String,
    /// Originating agent.
    pub origin: Agent,
    /// Human-readable title (first user prompt truncated, or agent-provided title).
    #[serde(default)]
    pub title: String,
    /// Epoch milliseconds.
    #[serde(default)]
    pub time_created: i64,
    #[serde(default)]
    pub time_updated: i64,
    /// The directory the session was working in, if known.
    #[serde(default)]
    pub directory: Option<String>,
    /// What the target puts in front of `title` (e.g. `[import:roo] `). `None`
    /// keeps the target's own default, which for opencode is `[<origin>] `.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_prefix: Option<String>,
    pub messages: Vec<Message>,
}

impl Session {
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Lossy shrink for `convert --compress`: drop every tool call and tool
    /// result, keeping only text/reasoning/attachments. Messages left with no
    /// parts are removed entirely.
    pub fn compress(&mut self) {
        for msg in &mut self.messages {
            msg.parts
                .retain(|p| !matches!(p, Part::ToolCall { .. } | Part::ToolResult { .. }));
        }
        self.messages.retain(|m| !m.parts.is_empty());
    }

    /// Rewrite path prefixes recorded on another machine to where the same
    /// files live here, e.g. `/Users/me/projects` -> `/home/me/projekty`.
    ///
    /// Applies to the session directory, to every string in tool-call inputs
    /// and to attachment paths. A prefix only matches on a path-component
    /// boundary, so `/a/b` rewrites `/a/b` and `/a/b/c` but not `/a/bc`. The
    /// first matching mapping wins, so list more specific prefixes first.
    pub fn map_paths(&mut self, maps: &[(String, String)]) {
        if maps.is_empty() {
            return;
        }
        if let Some(dir) = &mut self.directory {
            map_path(dir, maps);
        }
        for msg in &mut self.messages {
            for part in &mut msg.parts {
                match part {
                    Part::ToolCall { input: Some(input), .. } => map_json_paths(input, maps),
                    Part::Attachment { path: Some(path), .. } => map_path(path, maps),
                    _ => {}
                }
            }
        }
    }

    pub fn first_user_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .and_then(|m| {
                m.parts.iter().find_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
    }
}

/// Rewrite `path` in place with the first mapping whose prefix it starts with
/// on a component boundary. Tool inputs embed paths in longer strings too
/// (`cd /Users/me/x && make`), so a prefix also matches after whitespace, a
/// quote or `=`.
fn map_path(path: &mut String, maps: &[(String, String)]) {
    let mut out = String::with_capacity(path.len());
    let mut rest = path.as_str();
    let mut changed = false;
    let mut at_boundary = true;
    'scan: while !rest.is_empty() {
        if at_boundary {
            for (from, to) in maps {
                if let Some(after) = rest.strip_prefix(from.as_str())
                    && (after.is_empty() || after.starts_with(['/', '\\']) || from.ends_with(['/', '\\']))
                {
                    out.push_str(to);
                    rest = after;
                    changed = true;
                    at_boundary = false;
                    continue 'scan;
                }
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        at_boundary = c.is_whitespace() || matches!(c, '"' | '\'' | '=' | '(' | '`' | ':' | ',');
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if changed {
        *path = out;
    }
}

fn map_json_paths(value: &mut serde_json::Value, maps: &[(String, String)]) {
    match value {
        serde_json::Value::String(s) => map_path(s, maps),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| map_json_paths(v, maps)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| map_json_paths(v, maps)),
        _ => {}
    }
}

/// A format codec: read raw → canonical, and write canonical → raw.
pub trait Format {
    const AGENT: Agent;
    /// Human-readable name, e.g. "Claude Code".
    const NAME: &'static str;

    /// Where this agent stores sessions on disk (best-effort, platform-aware).
    fn session_dir() -> std::path::PathBuf;

    /// Read one session by id (or path) into the canonical representation.
    fn read(path: &Path) -> anyhow::Result<Session>;

    /// Write a canonical session out in this agent's format.
    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()>;

    /// List all sessions known to this agent. Returns (id, title, mtime, path).
    ///
    /// Walks up to three directory levels deep — several agents nest sessions
    /// (Codex: `sessions/<date>/<id>.jsonl`, Cline: `tasks/<id>/*.json`,
    /// Gemini: `tmp/<id>/*.json`) — and only returns session-looking files.
    fn list() -> Vec<SessionRef> {
        let dir = Self::session_dir();
        let mut out = Vec::new();
        walk_session_files(&dir, 3, &mut |p, mtime| {
            let id = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            out.push(SessionRef {
                agent: Self::AGENT,
                id,
                title: String::new(),
                path: p.to_path_buf(),
                mtime,
            });
        });
        out
    }
}

/// Recursively collect files with session-like extensions (json/jsonl/md), depth-limited.
pub fn walk_session_files(dir: &Path, depth: u32, f: &mut impl FnMut(&Path, i64)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if depth > 0 {
                walk_session_files(&p, depth - 1, f);
            }
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "json" | "jsonl" | "md") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        f(&p, mtime);
    }
}

/// Lightweight reference to a session on disk (no content loaded).
#[derive(Debug, Clone)]
pub struct SessionRef {
    pub agent: Agent,
    pub id: String,
    pub title: String,
    pub path: std::path::PathBuf,
    pub mtime: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_finds_nested_session_files() {
        let root = std::env::temp_dir().join(format!("baton-walk-test-{}", std::process::id()));
        let nested = root.join("2024-01-01").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("top.jsonl"), "{}").unwrap();
        std::fs::write(root.join("2024-01-01").join("mid.json"), "{}").unwrap();
        std::fs::write(nested.join("deep.md"), "x").unwrap();
        std::fs::write(nested.join("skipped.log"), "x").unwrap();

        let mut found = Vec::new();
        walk_session_files(&root, 3, &mut |p, _| {
            found.push(p.file_name().unwrap().to_string_lossy().to_string());
        });
        std::fs::remove_dir_all(&root).ok();

        found.sort();
        assert_eq!(found, ["deep.md", "mid.json", "top.jsonl"]);
    }

    #[test]
    fn map_paths_rewrites_directory_tool_inputs_and_attachments() {
        let mut session = Session {
            source_id: "s".into(),
            origin: Agent::Cline,
            title: "t".into(),
            time_created: 0,
            time_updated: 0,
            directory: Some("/Users/me/projects/app".into()),
            title_prefix: None,
            messages: vec![Message {
                role: Role::Assistant,
                parts: vec![
                    Part::ToolCall {
                        name: "execute_command".into(),
                        id: None,
                        input: Some(serde_json::json!({
                            "command": "cd /Users/me/projects/app && cat '/Users/me/projects/app/a'",
                            "files": [{ "path": "/Users/me/projectsX/b" }, { "path": "/Users/me/projects" }],
                        })),
                    },
                    Part::Attachment {
                        mime: "image/png".into(),
                        path: Some("/Volumes/x/shot.png".into()),
                        data: None,
                    },
                ],
                time_created: 0,
                origin: None,
                model: None,
            }],
        };
        session.map_paths(&[
            ("/Users/me/projects".into(), "/home/me/projekty".into()),
            ("/Volumes/x".into(), "/mnt/x".into()),
        ]);
        assert_eq!(session.directory.as_deref(), Some("/home/me/projekty/app"));
        let Part::ToolCall { input: Some(input), .. } = &session.messages[0].parts[0] else {
            panic!("tool call");
        };
        assert_eq!(
            input["command"],
            "cd /home/me/projekty/app && cat '/home/me/projekty/app/a'"
        );
        assert_eq!(input["files"][0]["path"], "/Users/me/projectsX/b", "only on a path boundary");
        assert_eq!(input["files"][1]["path"], "/home/me/projekty");
        let Part::Attachment { path, .. } = &session.messages[0].parts[1] else {
            panic!("attachment");
        };
        assert_eq!(path.as_deref(), Some("/mnt/x/shot.png"));
    }

    #[test]
    fn agent_parse_aliases() {
        assert_eq!(Agent::parse("Claude Code"), Some(Agent::ClaudeCode));
        assert_eq!(Agent::parse("roo"), Some(Agent::Cline));
        assert_eq!(Agent::parse("gemini"), Some(Agent::GeminiCli));
        assert_eq!(Agent::parse("nope"), None);
    }
}
