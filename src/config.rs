//! Register/unregister baton as an MCP server in each agent's config file.
//!
//! Each agent has a different config shape; we touch the minimal field set to add a
//! stdio MCP server entry named "baton". We preserve all existing config.

use std::path::Path;

use anyhow::Context;
use serde_json::{json, Value};

use crate::canonical::Agent;
use crate::detect::{server_command, DetectedAgent, SERVER_NAME};

/// Register baton in a single agent's config. Returns whether a change was made.
pub fn register(d: &DetectedAgent) -> anyhow::Result<bool> {
    match d.agent {
        Agent::ClaudeCode => register_claude(&d.config_path),
        Agent::Opencode => register_opencode(&d.config_path),
        Agent::Cursor => register_cursor(&d.config_path),
        Agent::Continue => register_continue(&d.config_path),
        Agent::Cline => register_cline(&d.config_path),
        Agent::Zed => register_zed(&d.config_path),
        Agent::Codex => register_codex(&d.config_path),
        Agent::GeminiCli => register_gemini(&d.config_path),
        Agent::Aider => register_aider(&d.config_path),
        Agent::Unknown => Ok(false),
    }
}

/// Remove baton from a single agent's config.
pub fn unregister(d: &DetectedAgent) -> anyhow::Result<bool> {
    match d.agent {
        Agent::ClaudeCode => unregister_key(&d.config_path, &["mcpServers", SERVER_NAME]),
        Agent::Opencode => unregister_key(&d.config_path, &["mcp", "servers", SERVER_NAME]),
        Agent::Cursor => unregister_key(&d.config_path, &["mcpServers", SERVER_NAME]),
        Agent::Continue => unregister_key(&d.config_path, &["mcpServers", SERVER_NAME]),
        Agent::Cline => unregister_key(&d.config_path, &["mcpServers", SERVER_NAME]),
        Agent::Zed => unregister_key(&d.config_path, &["context_servers", SERVER_NAME]),
        Agent::Codex => unregister_codex(&d.config_path),
        Agent::GeminiCli => unregister_key(&d.config_path, &["mcpServers", SERVER_NAME]),
        Agent::Aider => Ok(false),
        Agent::Unknown => Ok(false),
    }
}

/// Check whether baton is actually registered in an agent's config (parses the
/// config instead of substring-matching, so a stray "baton" elsewhere doesn't count).
pub fn is_registered(d: &DetectedAgent) -> anyhow::Result<bool> {
    let key_path: &[&str] = match d.agent {
        Agent::Opencode => &["mcp", "servers", SERVER_NAME],
        Agent::Zed => &["context_servers", SERVER_NAME],
        Agent::Codex => {
            let raw = std::fs::read_to_string(&d.config_path).unwrap_or_default();
            return Ok(raw.contains("[mcp_servers.baton]"));
        }
        Agent::Aider | Agent::Unknown => return Ok(false),
        _ => &["mcpServers", SERVER_NAME],
    };
    // Lenient load: Zed settings are JSONC; comments must not break detection.
    let root = match load_json_lenient(&d.config_path) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let mut cur = &root;
    for k in key_path {
        match cur.get(k) {
            Some(v) => cur = v,
            None => return Ok(false),
        }
    }
    Ok(true)
}

fn server_entry() -> Value {
    let cmd = server_command();
    json!({
        "command": cmd[0],
        "args": cmd[1..].to_vec(),
    })
}

// --- per-agent registrars ---

fn register_claude(path: &Path) -> anyhow::Result<bool> {
    let mut root = load_json_or_default(path)?;
    let servers = root
        .as_object_mut()
        .context("claude config root not an object")?
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcpServers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_opencode(path: &Path) -> anyhow::Result<bool> {
    let mut root = load_json_or_default(path)?;
    let obj = root
        .as_object_mut()
        .context("opencode.json not an object")?;
    let mcp = obj
        .entry("mcp".to_string())
        .or_insert(json!({}));
    let mcp = mcp.as_object_mut().context("mcp not object")?;
    let servers = mcp
        .entry("servers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcp.servers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_cursor(path: &Path) -> anyhow::Result<bool> {
    ensure_parent(path)?;
    let mut root = load_json_or_default(path)?;
    if root.is_null() {
        root = json!({});
    }
    let obj = root
        .as_object_mut()
        .context("cursor mcp.json not an object")?;
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcpServers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_continue(path: &Path) -> anyhow::Result<bool> {
    let mut root = load_json_or_default(path)?;
    let obj = root
        .as_object_mut()
        .context("continue config not an object")?;
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcpServers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_cline(path: &Path) -> anyhow::Result<bool> {
    ensure_parent(path)?;
    let mut root = load_json_or_default(path)?;
    if root.is_null() {
        root = json!({});
    }
    let obj = root
        .as_object_mut()
        .context("cline config not an object")?;
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcpServers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_zed(path: &Path) -> anyhow::Result<bool> {
    // Zed settings.json is JSONC. We can *parse* it leniently, but rewriting through
    // serde would destroy the user's comments — so if strict JSON parsing fails,
    // check registration leniently and otherwise refuse with a manual hint.
    if load_json_or_default(path).is_err() {
        let lenient = load_json_lenient(path)?;
        if lenient
            .get("context_servers")
            .and_then(|s| s.get(SERVER_NAME))
            .is_some()
        {
            return Ok(false); // already registered; nothing to rewrite
        }
        anyhow::bail!(
            "Zed settings.json contains comments (JSONC), which baton won't rewrite — add the baton entry under \"context_servers\" manually: {}",
            path.display()
        );
    }
    let mut root = load_json_or_default(path)?;
    let obj = root
        .as_object_mut()
        .context("zed settings not an object")?;
    let servers = obj
        .entry("context_servers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("context_servers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_codex(path: &Path) -> anyhow::Result<bool> {
    // Codex uses TOML; we do a conservative line-based insert under [mcp_servers].
    ensure_parent(path)?;
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let header = "[mcp_servers.baton]";
    if raw.contains("mcp_servers.baton") || raw.contains(header) {
        return Ok(false);
    }
    let cmd = server_command();
    let entry = format!(
        "\n{header}\ncommand = {}\nargs = [{}]\n",
        toml_string(&cmd[0]),
        cmd[1..]
            .iter()
            .map(|a| toml_string(a))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let new_raw = if raw.trim().is_empty() {
        entry
    } else {
        format!("{}\n{}", raw.trim_end(), entry)
    };
    std::fs::write(path, new_raw).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn unregister_codex(path: &Path) -> anyhow::Result<bool> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    if !raw.contains("mcp_servers.baton") {
        return Ok(false);
    }
    // Naive: drop everything from the [mcp_servers.baton] header until the next blank-line-
    // delimited section or EOF. Good enough for a single-managed entry.
    let mut out = String::new();
    let mut skipping = false;
    for line in raw.lines() {
        if line.trim() == "[mcp_servers.baton]" {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim().is_empty() || line.trim_start().starts_with('[') {
                skipping = false;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn register_gemini(path: &Path) -> anyhow::Result<bool> {
    let mut root = load_json_or_default(path)?;
    let obj = root
        .as_object_mut()
        .context("gemini settings not an object")?;
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert(json!({}));
    let servers = servers.as_object_mut().context("mcpServers not object")?;
    if servers.contains_key(SERVER_NAME) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_string(), server_entry());
    save_json(path, &root)
}

fn register_aider(_path: &Path) -> anyhow::Result<bool> {
    // Aider doesn't support MCP servers. Nothing to do.
    Ok(false)
}

// --- helpers ---

fn load_json_or_default(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// Parse a JSON file tolerating JSONC extensions (comments, trailing commas).
/// Read-only — never use the result to rewrite the file, it drops comments.
fn load_json_lenient(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&strip_jsonc(&raw))
        .with_context(|| format!("parsing {} (lenient)", path.display()))
}

/// Strip // and /* */ comments plus trailing commas, respecting string literals.
fn strip_jsonc(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
        } else if b == b'"' {
            in_string = true;
            out.push(b);
            i += 1;
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else if b == b',' {
            // Drop the comma if the next non-whitespace/non-comment byte closes a scope.
            let rest = &s[i + 1..];
            let stripped_rest = strip_jsonc_lookahead(rest);
            if matches!(stripped_rest.trim_start().as_bytes().first(), Some(b'}') | Some(b']')) {
                i += 1;
                continue;
            }
            out.push(b);
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Lookahead helper: strip comments from the head of `rest` (no string handling
/// needed — we only inspect up to the first meaningful byte).
fn strip_jsonc_lookahead(rest: &str) -> String {
    let mut r = rest;
    loop {
        let t = r.trim_start();
        if let Some(after) = t.strip_prefix("//") {
            r = after.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        } else if let Some(after) = t.strip_prefix("/*") {
            r = after.split_once("*/").map(|(_, tail)| tail).unwrap_or("");
        } else {
            return t.to_string();
        }
    }
}

/// Escape a value as a TOML basic string (handles Windows backslash paths).
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn save_json(path: &Path, v: &Value) -> anyhow::Result<bool> {
    ensure_parent(path)?;
    // Rewriting a live agent config: keep a one-shot backup of the original.
    if path.exists() {
        let bak = path.with_extension("json.baton-bak");
        let _ = std::fs::copy(path, bak);
    }
    let pretty = serde_json::to_string_pretty(v)?;
    std::fs::write(path, pretty).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(p) = path.parent()
        && !p.as_os_str().is_empty() && !p.exists() {
            std::fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))?;
        }
    Ok(())
}

fn unregister_key(path: &Path, keys: &[&str]) -> anyhow::Result<bool> {
    let mut root = load_json_or_default(path)?;
    if root.is_null() {
        return Ok(false);
    }
    let removed = remove_nested(&mut root, keys);
    if removed {
        save_json(path, &root)?;
    }
    Ok(removed)
}

fn remove_nested(root: &mut Value, keys: &[&str]) -> bool {
    if keys.is_empty() {
        return false;
    }
    let Some(obj) = root.as_object_mut() else {
        return false;
    };
    if keys.len() == 1 {
        obj.remove(keys[0]).is_some()
    } else {
        let Some(child) = obj.get_mut(keys[0]) else {
            return false;
        };
        remove_nested(child, &keys[1..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baton-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn detected(agent: Agent, path: &Path) -> DetectedAgent {
        DetectedAgent {
            agent,
            config_path: path.to_path_buf(),
            config_exists: path.exists(),
            has_sessions: false,
        }
    }

    #[test]
    fn register_unregister_json_round_trip() {
        let path = tmp("claude.json");
        std::fs::write(&path, r#"{"other": {"keep": true}}"#).unwrap();
        let d = detected(Agent::ClaudeCode, &path);

        assert!(register(&d).unwrap());
        assert!(is_registered(&d).unwrap());
        // second register is a no-op
        assert!(!register(&d).unwrap());

        // existing config preserved
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["other"]["keep"], true);
        assert!(root["mcpServers"]["baton"]["command"].is_string());

        assert!(unregister(&d).unwrap());
        assert!(!is_registered(&d).unwrap());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn is_registered_ignores_unrelated_baton_mentions() {
        let path = tmp("gemini.json");
        std::fs::write(&path, r#"{"note": "I love baton"}"#).unwrap();
        let d = detected(Agent::GeminiCli, &path);
        assert!(!is_registered(&d).unwrap());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn codex_toml_register_unregister() {
        let path = tmp("config.toml");
        std::fs::write(&path, "model = \"o3\"\n").unwrap();
        let d = detected(Agent::Codex, &path);

        assert!(register(&d).unwrap());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[mcp_servers.baton]"));
        assert!(raw.contains("model = \"o3\""));
        assert!(is_registered(&d).unwrap());

        assert!(unregister(&d).unwrap());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("mcp_servers.baton"));
        assert!(raw.contains("model = \"o3\""));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn toml_string_escapes_windows_paths() {
        assert_eq!(toml_string(r"C:\bin\baton.exe"), r#""C:\\bin\\baton.exe""#);
    }
}

#[cfg(test)]
mod jsonc_tests {
    use super::*;

    #[test]
    fn strips_comments_and_trailing_commas() {
        let jsonc = r#"
{
  // line comment
  "theme": "dark", /* block comment */
  "url": "https://example.com//not-a-comment",
  "list": [1, 2, ],
  "nested": {
    "a": 1,
  },
}
"#;
        let v: Value = serde_json::from_str(&strip_jsonc(jsonc)).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["url"], "https://example.com//not-a-comment");
        assert_eq!(v["list"][1], 2);
        assert_eq!(v["nested"]["a"], 1);
    }

    #[test]
    fn zed_jsonc_registration_detected_but_not_rewritten() {
        let dir = std::env::temp_dir().join(format!("baton-jsonc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        // commented settings WITHOUT baton → register must refuse, file untouched
        let original = "{\n  // my settings\n  \"theme\": \"dark\",\n}\n";
        std::fs::write(&path, original).unwrap();
        let d = DetectedAgent {
            agent: Agent::Zed,
            config_path: path.clone(),
            config_exists: true,
            has_sessions: false,
        };
        assert!(register(&d).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!is_registered(&d).unwrap());

        // commented settings WITH baton → detected as registered, no error
        let with_baton = "{\n  // hi\n  \"context_servers\": { \"baton\": { \"command\": \"baton\" } },\n}\n";
        std::fs::write(&path, with_baton).unwrap();
        assert!(is_registered(&d).unwrap());
        assert!(!register(&d).unwrap()); // no-op, no rewrite
        assert_eq!(std::fs::read_to_string(&path).unwrap(), with_baton);

        std::fs::remove_dir_all(&dir).ok();
    }
}
