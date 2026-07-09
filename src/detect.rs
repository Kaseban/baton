//! Detect installed coding agents and locate their MCP config files.
//!
//! Each agent stores its MCP server config in a well-known location. We resolve those
//! paths per-platform and report which agents appear installed (by checking for the
//! config file OR a known binary/session dir on PATH / disk).

use std::path::PathBuf;

use crate::canonical::Agent;

/// One detected agent + the config file we'd touch to register baton.
#[derive(Debug, Clone)]
pub struct DetectedAgent {
    pub agent: Agent,
    /// Path to the file that holds MCP server config (may not exist yet).
    pub config_path: PathBuf,
    /// Whether the config file already exists.
    pub config_exists: bool,
    /// Whether a session directory was found (stronger signal the agent is actually used).
    pub has_sessions: bool,
}

impl DetectedAgent {
    pub fn is_installed(&self) -> bool {
        self.config_exists || self.has_sessions
    }
}

/// Scan the system for every agent we know about.
pub fn detect_all() -> Vec<DetectedAgent> {
    let mut out = Vec::new();
    for agent in crate::formats::ALL_AGENTS {
        let (config_path, session_dir) = paths_for(*agent);
        let config_exists = config_path.as_deref().is_some_and(|p| p.exists());
        let has_sessions = session_dir.as_deref().is_some_and(|p| p.exists());
        if let Some(cp) = config_path {
            out.push(DetectedAgent {
                agent: *agent,
                config_path: cp,
                config_exists,
                has_sessions,
            });
        }
    }
    out
}

/// Only return agents that look installed (config exists or sessions present).
pub fn detect_installed() -> Vec<DetectedAgent> {
    detect_all().into_iter().filter(|d| d.is_installed()).collect()
}

/// Return the (config_path, session_dir) pair for an agent, if we know it.
pub fn paths_for(agent: Agent) -> (Option<PathBuf>, Option<PathBuf>) {
    let home = dirs::home_dir();
    let config_path = match agent {
        Agent::ClaudeCode => home.as_ref().map(|h| h.join(".claude.json")),
        Agent::Opencode => {
            // opencode.json is per-project; we register in the cwd's opencode.json
            std::env::current_dir().ok().map(|d| d.join("opencode.json"))
        }
        Agent::Codex => home.as_ref().map(|h| h.join(".codex").join("config.toml")),
        Agent::Cursor => {
            // .cursor/mcp.json is per-project
            std::env::current_dir().ok().map(|d| d.join(".cursor").join("mcp.json"))
        }
        Agent::Continue => home.as_ref().map(|h| h.join(".continue").join("config.json")),
        Agent::Cline => {
            // Cline stores MCP config in VS Code/Cursor settings dir
            std::env::current_dir().ok().map(|d| {
                d.join(".vscode")
                    .join("cline_mcp_settings.json")
            })
        }
        Agent::Zed => {
            let base = if cfg!(target_os = "macos") {
                home.as_ref().map(|h| {
                    h.join("Library")
                        .join("Application Support")
                        .join("Zed")
                })
            } else if cfg!(target_os = "windows") {
                dirs::config_dir().map(|d| d.join("Zed"))
            } else {
                dirs::config_dir().map(|d| d.join("zed"))
            };
            base.map(|b| b.join("settings.json"))
        }
        Agent::Aider => home.as_ref().map(|h| h.join(".aider.conf.yml")),
        Agent::GeminiCli => home.as_ref().map(|h| h.join(".gemini").join("settings.json")),
        Agent::Unknown => None,
    };

    // Session dirs come from each format's own `session_dir()` so detection and
    // listing can never disagree. Aider is per-repo: look for its history file in cwd.
    use crate::canonical::Format as _;
    let session_dir = match agent {
        Agent::ClaudeCode => Some(crate::formats::claude_code::ClaudeCode::session_dir()),
        Agent::Opencode => Some(crate::formats::opencode::Opencode::session_dir()),
        Agent::Codex => Some(crate::formats::codex::Codex::session_dir()),
        Agent::Cursor => Some(crate::formats::cursor::Cursor::session_dir()),
        Agent::Continue => Some(crate::formats::continue_dev::ContinueDev::session_dir()),
        Agent::Cline => Some(crate::formats::cline::Cline::session_dir()),
        Agent::Zed => Some(crate::formats::zed::Zed::session_dir()),
        Agent::Aider => std::env::current_dir()
            .ok()
            .map(|d| d.join(".aider.chat.history.md")),
        Agent::GeminiCli => Some(crate::formats::gemini_cli::GeminiCli::session_dir()),
        Agent::Unknown => None,
    };

    (config_path, session_dir)
}

/// Sniff a path and guess which agent produced it.
pub fn detect_at_path(path: &std::path::Path) -> Agent {
    // Check if it lives inside a known agent's session dir.
    let path_str = path.to_string_lossy();
    let home = dirs::home_dir();

    if let Some(h) = &home {
        let claude_dir = h.join(".claude").join("projects");
        if path_str.starts_with(&claude_dir.to_string_lossy().to_string()) {
            return Agent::ClaudeCode;
        }
        let codex_dir = h.join(".codex").join("sessions");
        if path_str.starts_with(&codex_dir.to_string_lossy().to_string()) {
            return Agent::Codex;
        }
        let gemini_dir = h.join(".gemini");
        if path_str.starts_with(&gemini_dir.to_string_lossy().to_string()) {
            return Agent::GeminiCli;
        }
    }

    if path_str.contains(".continue/sessions") {
        return Agent::Continue;
    }
    if path_str.contains(".cline") || path_str.contains("cline_mcp") {
        return Agent::Cline;
    }
    if path_str.contains("Zed") && path_str.contains("assistant") {
        return Agent::Zed;
    }
    if path_str.contains(".cursor") {
        return Agent::Cursor;
    }
    if path_str.contains("opencode") || path_str.contains("ses_") {
        return Agent::Opencode;
    }

    // Heuristic: read file content and detect by *structure*, never by substring
    // search over the whole file. Transcripts of coding sessions routinely contain
    // other agents' markers (e.g. "ses_", "#### USER") inside message text, so
    // substring sniffing misdetects. Parse and check top-level shape instead.
    if path.is_file()
        && let Ok(content) = std::fs::read_to_string(path)
            && let Some(agent) = sniff_structure(&content) {
                return agent;
            }

    // Filename patterns
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.starts_with("ses_") {
            return Agent::Opencode;
        }
        // UUID.jsonl pattern typical of Claude Code
        if let Some(stem) = name.strip_suffix(".jsonl")
            && stem.len() == 36 && stem.chars().filter(|c| *c == '-').count() == 4 {
                return Agent::ClaudeCode;
            }
    }

    Agent::Unknown
}

/// Structural content sniffing: parse JSON/JSONL and inspect top-level shape.
/// Returns None when the content doesn't match any known session structure.
fn sniff_structure(content: &str) -> Option<Agent> {
    // JSONL formats: judge by the first non-empty line.
    let first_line = content.lines().find(|l| !l.trim().is_empty())?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(first_line.trim())
        && let Some(obj) = v.as_object() {
            // Codex rollout: {"timestamp":..,"type":"session_meta","payload":{..}}
            if obj.get("type").and_then(|t| t.as_str()) == Some("session_meta")
                && obj.contains_key("payload")
            {
                return Some(Agent::Codex);
            }
            // Claude Code JSONL: top-level sessionId/leafUuid on each record.
            if obj.contains_key("sessionId") || obj.contains_key("leafUuid") {
                return Some(Agent::ClaudeCode);
            }
            // OpenCode export: {"info":{"id":"ses_.."},"messages":[..]} (single JSON,
            // but a compact export fits on one line).
            if obj.contains_key("info") && obj.contains_key("messages") {
                return Some(Agent::Opencode);
            }
        }
    // Whole-file JSON (pretty-printed documents span many lines).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
        match &v {
            serde_json::Value::Array(items) => {
                if let Some(first) = items.iter().find_map(|i| i.as_object()) {
                    // Gemini checkpoint: [{"role":"user","parts":[..]}, ..]
                    if first.contains_key("role") && first.contains_key("parts") {
                        return Some(Agent::GeminiCli);
                    }
                    // Zed thread: [{"User":{..}} | {"Agent":{..}}, ..]
                    if first.len() == 1
                        && first
                            .keys()
                            .all(|k| k == "User" || k == "Agent" || k == "System")
                    {
                        return Some(Agent::Zed);
                    }
                }
            }
            serde_json::Value::Object(obj)
                if obj.contains_key("info") && obj.contains_key("messages") => {
                    return Some(Agent::Opencode);
                }
            _ => {}
        }
    }
    // Aider markdown history: anchor to leading lines, never substring-anywhere —
    // transcripts about aider legitimately contain these markers mid-file.
    if content.starts_with("# aider chat started at")
        || content
            .lines()
            .take(5)
            .any(|l| l == "#### USER" || l == "#### ASSISTANT")
    {
        return Some(Agent::Aider);
    }
    None
}

/// The command we register as the MCP server invocation.
/// We use the absolute path to this binary if discoverable, else `baton`.
pub fn server_command() -> Vec<String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("baton"));
    vec![
        exe.to_string_lossy().to_string(),
        "serve".to_string(),
    ]
}

/// The canonical server name we register under in every agent config.
pub const SERVER_NAME: &str = "baton";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_patterns() {
        assert_eq!(
            detect_at_path(std::path::Path::new("/x/ses_abc123.json")),
            Agent::Opencode
        );
        assert_eq!(
            detect_at_path(std::path::Path::new(
                "/x/fa88b429-1234-1234-1234-123456789abc.jsonl"
            )),
            Agent::ClaudeCode
        );
    }

    #[test]
    fn content_sniffing() {
        let dir = std::env::temp_dir().join(format!("baton-detect-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let aider = dir.join("h.md");
        std::fs::write(&aider, "# aider chat started at 2024-01-01\n\n#### hi\nok\n").unwrap();
        assert_eq!(detect_at_path(&aider), Agent::Aider);

        let claude = dir.join("c.txt");
        std::fs::write(&claude, r#"{"sessionId":"x","type":"user"}"#).unwrap();
        assert_eq!(detect_at_path(&claude), Agent::ClaudeCode);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Transcripts *about* coding agents contain other agents' markers ("ses_",
    /// "#### USER", "sessionID") inside message text. Detection must key on
    /// structure, not substrings anywhere in the file.
    #[test]
    fn content_sniffing_adversarial() {
        let dir =
            std::env::temp_dir().join(format!("baton-detect-adv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Codex rollout whose payload text mentions opencode markers.
        let codex = dir.join("rollout.jsonl");
        std::fs::write(
            &codex,
            concat!(
                r#"{"timestamp":"2026-07-06T02:31:14.245Z","type":"session_meta","payload":{"id":"x","instructions":null}}"#,
                "\n",
                r#"{"timestamp":"2026-07-06T02:31:15.000Z","type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"the id is ses_abc and \"sessionID\" and #### USER"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(detect_at_path(&codex), Agent::Codex);

        // Gemini checkpoint whose text mentions ses_ / aider headers.
        let gemini = dir.join("checkpoint.json");
        std::fs::write(
            &gemini,
            r#"[{"role":"user","parts":[{"text":"ses_123 and #### ASSISTANT"}]},{"role":"model","parts":[{"text":"ok"}]}]"#,
        )
        .unwrap();
        assert_eq!(detect_at_path(&gemini), Agent::GeminiCli);

        // Zed thread mentioning opencode session ids.
        let zed = dir.join("thread.json");
        std::fs::write(
            &zed,
            r#"[{"User":{"content":[{"Text":{"text":"see ses_999"}}]}},{"Agent":{"content":[{"Text":{"text":"ok"}}]}}]"#,
        )
        .unwrap();
        assert_eq!(detect_at_path(&zed), Agent::Zed);

        // OpenCode export still detected structurally.
        let oc = dir.join("export.json");
        std::fs::write(
            &oc,
            r#"{"info":{"id":"ses_b1112df89652457ba15437c4"},"messages":[]}"#,
        )
        .unwrap();
        assert_eq!(detect_at_path(&oc), Agent::Opencode);

        std::fs::remove_dir_all(&dir).ok();
    }
}
