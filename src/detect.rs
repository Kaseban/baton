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
        Agent::Aider => home.as_ref().map(|h| h.join(".aider").join("conf.yml")),
        Agent::GeminiCli => home.as_ref().map(|h| h.join(".gemini").join("settings.json")),
        Agent::Unknown => None,
    };

    let session_dir = match agent {
        Agent::ClaudeCode => home.as_ref().map(|h| h.join(".claude").join("projects")),
        Agent::Opencode => dirs::data_local_dir().map(|d| d.join("opencode").join("storage")),
        Agent::Codex => home.as_ref().map(|h| h.join(".codex").join("sessions")),
        Agent::Cursor => home.as_ref().map(|h| h.join(".cursor")),
        Agent::Continue => home.as_ref().map(|h| h.join(".continue").join("sessions")),
        Agent::Cline => Some(PathBuf::from(".cline")),
        Agent::Zed => {
            if cfg!(target_os = "macos") {
                home.as_ref().map(|h| {
                    h.join("Library")
                        .join("Application Support")
                        .join("Zed")
                        .join("assistant")
                        .join("conversations")
                })
            } else {
                dirs::data_local_dir().map(|d| {
                    d.join(if cfg!(target_os = "windows") { "Zed" } else { "zed" })
                        .join("assistant")
                        .join("conversations")
                })
            }
        }
        Agent::Aider => Some(PathBuf::from(".")),
        Agent::GeminiCli => home.as_ref().map(|h| h.join(".gemini")),
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

    // Heuristic: read file content and look for patterns.
    if path.is_file() {
        if let Ok(content) = std::fs::read_to_string(path) {
            // Claude Code: JSONL with "type":"user"/"assistant" + "message":{"role"
            if content.contains("\"leafUuid\"") || content.contains("\"sessionId\"") {
                return Agent::ClaudeCode;
            }
            // OpenCode: JSON with "sessionID" or "msg_" / "prt_" prefixes
            if content.contains("\"sessionID\"") || content.contains("ses_") {
                return Agent::Opencode;
            }
            // Aider: markdown with #### headers
            if content.contains("#### user") || content.contains("#### assistant") {
                return Agent::Aider;
            }
        }
    }

    // Filename patterns
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.starts_with("ses_") {
            return Agent::Opencode;
        }
        if name.ends_with(".jsonl") && name.len() == 41 {
            // UUID.jsonl pattern typical of Claude Code
            return Agent::ClaudeCode;
        }
    }

    Agent::Unknown
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
