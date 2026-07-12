//! `baton watch` — auto-failover.
//!
//! Polls the current project's sessions for quota-death ("usage limit reached")
//! and passes the baton to another agent: convert the transcript, import it,
//! and hand the user a ready-to-run resume command.
//!
//! Two modes:
//!   - interactive (default): list quota-dead sessions, let the user pick which
//!     to fail over.
//!   - `--auto`: unattended — only sessions opted in via the `failover_opt_in`
//!     MCP tool (or `baton watch` state file) are failed over, no prompt.
//!
//! Detection is per-format:
//!   - Claude Code: the newest assistant event in each project transcript, when
//!     flagged `isApiErrorMessage` with limit-shaped text, marks the session dead.
//!   - OpenCode: the newest message per session (via `opencode db`), when it
//!     carries a limit-shaped `error`, marks the session dead.
//!
//! Run it from the project directory you're working in — the claude-code import
//! path encodes the process cwd, so failing over *to* claude-code from elsewhere
//! would land the session under the wrong project.

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::canonical::Agent;
use crate::convert;

/// Quota errors older than this are assumed reset (session limits reset within
/// 5 hours) and are not surfaced on a fresh `baton watch` start.
const STALE_MS: i64 = 6 * 60 * 60 * 1000;
/// How much of a transcript tail to inspect for the newest assistant event.
const TAIL_BYTES: u64 = 64 * 1024;

pub struct WatchOpts {
    pub auto: bool,
    pub once: bool,
    pub interval: u64,
    pub to: Option<Agent>,
    pub project: Option<PathBuf>,
}

/// Persistent failover state, shared between `baton watch` and the MCP tools.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// `"<agent>:<session-id>"` entries opted in for unattended (`--auto`) failover.
    #[serde(default)]
    pub opt_in: Vec<String>,
    /// `"<agent>:<session-id>"` → id of the last quota-error event already
    /// surfaced, so the same death only fires once.
    #[serde(default)]
    pub handled: HashMap<String, String>,
}

pub fn state_path() -> PathBuf {
    let dir = std::env::var_os("BATON_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("baton")
        });
    dir.join("failover.json")
}

impl State {
    pub fn key(agent: Agent, session_id: &str) -> String {
        format!("{agent}:{session_id}")
    }

    pub fn load() -> Self {
        Self::load_from(&state_path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&state_path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// A session whose newest event is a quota error.
#[derive(Debug)]
struct Dead {
    agent: Agent,
    session_id: String,
    /// Transcript path for file-based agents (claude-code); opencode exports on demand.
    path: Option<PathBuf>,
    /// Id of the error event — dedup key in [`State::handled`].
    error_id: String,
    error_text: String,
}

pub fn run(opts: WatchOpts) -> anyhow::Result<()> {
    let project = match &opts.project {
        Some(p) => p.clone(),
        None => std::env::current_dir().context("getting cwd")?,
    };
    let project = std::fs::canonicalize(&project).unwrap_or(project);
    eprintln!(
        "baton watch — {} · {} · every {}s (ctrl-c to stop)",
        project.display(),
        if opts.auto { "auto (opted-in sessions only)" } else { "interactive" },
        opts.interval
    );
    loop {
        if let Err(e) = scan(&project, &opts) {
            eprintln!("watch: {e:#}");
        }
        if opts.once {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(opts.interval.max(1)));
    }
    Ok(())
}

fn scan(project: &Path, opts: &WatchOpts) -> anyhow::Result<()> {
    let mut state = State::load();
    let mut dead = scan_claude(project);
    dead.extend(scan_opencode(project));
    dead.retain(|d| {
        state.handled.get(&State::key(d.agent, &d.session_id)) != Some(&d.error_id)
    });
    if dead.is_empty() {
        return Ok(());
    }

    eprintln!();
    for d in &dead {
        eprintln!("⚠ {} session {} hit its limit: {}", d.agent, d.session_id, d.error_text);
    }

    let chosen: Vec<&Dead> = if opts.auto {
        let (go, skip): (Vec<&Dead>, Vec<&Dead>) = dead
            .iter()
            .partition(|d| state.opt_in.contains(&State::key(d.agent, &d.session_id)));
        for d in skip {
            eprintln!("· skipping {} (not opted in — use the failover_opt_in MCP tool)", d.session_id);
        }
        go
    } else {
        pick(&dead)?
    };

    for d in chosen {
        match failover(d, opts.to) {
            Ok(()) => eprintln!("✓ failed over {} {}", d.agent, d.session_id),
            Err(e) => eprintln!("✗ failover failed for {}: {e:#}", d.session_id),
        }
    }
    // Every surfaced death is marked handled — chosen or skipped — so one error
    // event prompts exactly once. A *new* error event on the same session refires,
    // and failover_opt_in clears the entry so opting in after a death re-arms it.
    // Merge into freshly-loaded state: an opt-in written by the MCP tool while we
    // were scanning must not be clobbered by our stale copy.
    let mut fresh = State::load();
    for d in &dead {
        fresh
            .handled
            .insert(State::key(d.agent, &d.session_id), d.error_id.clone());
    }
    fresh.save()
}

/// Interactive selection. Accepts "a" (all), "s" (skip), or comma-separated numbers.
fn pick(dead: &[Dead]) -> anyhow::Result<Vec<&Dead>> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "not running in a terminal — use --auto with opted-in sessions"
    );
    for (i, d) in dead.iter().enumerate() {
        eprintln!("{:>3}) {} {}", i + 1, d.agent, d.session_id);
    }
    loop {
        eprint!("fail over which? [1-{}, a=all, s=skip]: ", dead.len());
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(Vec::new());
        }
        let line = line.trim().to_ascii_lowercase();
        match line.as_str() {
            "a" => return Ok(dead.iter().collect()),
            "s" | "q" | "" => return Ok(Vec::new()),
            _ => {
                let picks: Option<Vec<&Dead>> = line
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<usize>()
                            .ok()
                            .filter(|n| (1..=dead.len()).contains(n))
                            .map(|n| &dead[n - 1])
                    })
                    .collect();
                match picks {
                    Some(p) => return Ok(p),
                    None => eprintln!("enter numbers 1-{}, 'a', or 's'", dead.len()),
                }
            }
        }
    }
}

/// Convert the dead session and import it into the target agent.
fn failover(d: &Dead, to_override: Option<Agent>) -> anyhow::Result<()> {
    let to = match to_override {
        Some(a) => a,
        None => match d.agent {
            Agent::ClaudeCode => Agent::Opencode,
            Agent::Opencode => Agent::ClaudeCode,
            a => anyhow::bail!("no default failover target for {a} — pass --to"),
        },
    };
    let input = match d.agent {
        Agent::ClaudeCode => d.path.clone().context("missing transcript path")?,
        Agent::Opencode => export_opencode(&d.session_id)?,
        a => anyhow::bail!("failover from {a} not supported yet"),
    };
    let ext = if to == Agent::ClaudeCode { "jsonl" } else { "json" };
    let out = std::env::temp_dir().join(format!("baton-failover-{to}-{}.{ext}", d.session_id));
    convert::convert(d.agent, to, &input, Some(&out))?;
    convert::import_to_target(to, &out)
}

/// `opencode export <id>` → temp file (opencode keeps live sessions in SQLite;
/// export is the supported way out).
fn export_opencode(session_id: &str) -> anyhow::Result<PathBuf> {
    let out = std::process::Command::new("opencode")
        .args(["export", session_id])
        .output()
        .context("running `opencode export`")?;
    anyhow::ensure!(
        out.status.success(),
        "`opencode export {session_id}` exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let path = std::env::temp_dir().join(format!("baton-export-{session_id}.json"));
    std::fs::write(&path, &out.stdout).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

// --- claude-code detection ---

fn scan_claude(project: &Path) -> Vec<Dead> {
    let encoded = convert::encode_claude_project_dir(project);
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
        .join(encoded);
    scan_claude_dir(&dir)
}

fn scan_claude_dir(dir: &Path) -> Vec<Dead> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if now - mtime > STALE_MS {
            continue;
        }
        let Ok(lines) = tail_lines(&p, TAIL_BYTES) else {
            continue;
        };
        if let Some((error_id, error_text)) = claude_dead(&lines) {
            out.push(Dead {
                agent: Agent::ClaudeCode,
                session_id: p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string(),
                path: Some(p),
                error_id,
                error_text,
            });
        }
    }
    out
}

/// The newest assistant event decides liveness: if it's a limit-shaped API
/// error the session is dead; anything else means it recovered.
fn claude_dead(lines: &[String]) -> Option<(String, String)> {
    for l in lines.iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let text = claude_quota_text(&v)?;
        let error_id = v
            .get("uuid")
            .and_then(|u| u.as_str())
            .unwrap_or(&text)
            .to_string();
        return Some((error_id, text));
    }
    None
}

/// Limit-shaped API-error text of an assistant event, if any.
/// Real shapes: "You've hit your session limit · resets 5:50pm (America/Toronto)",
/// "Claude AI usage limit reached|1712345678".
fn claude_quota_text(v: &serde_json::Value) -> Option<String> {
    if v.get("isApiErrorMessage").and_then(|b| b.as_bool()) != Some(true) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .find_map(|b| b.get("text").and_then(|t| t.as_str()))?
            .to_string(),
        _ => return None,
    };
    let lower = text.to_ascii_lowercase();
    (lower.contains("limit") && (lower.contains("reached") || lower.contains("reset")))
        .then_some(text)
}

/// Last `max_bytes` of a file as lines, dropping a leading partial line.
fn tail_lines(path: &Path, max_bytes: u64) -> std::io::Result<Vec<String>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(std::io::SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let s = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = s.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Ok(lines)
}

// --- opencode detection ---

fn scan_opencode(project: &Path) -> Vec<Dead> {
    let since = chrono::Utc::now().timestamp_millis() - STALE_MS;
    // Newest message per session, recent only. opencode keeps sessions in
    // SQLite; `opencode db --format json` is the supported zero-dep way in.
    let sql = format!(
        "SELECT m.session_id AS sid, m.id AS mid, m.data AS data, s.directory AS dir \
         FROM message m JOIN session s ON s.id = m.session_id \
         WHERE m.id = (SELECT id FROM message m2 WHERE m2.session_id = m.session_id \
                       ORDER BY m2.time_created DESC, m2.id DESC LIMIT 1) \
           AND m.time_created > {since}"
    );
    let Ok(out) = std::process::Command::new("opencode")
        .args(["db", &sql, "--format", "json"])
        .output()
    else {
        return Vec::new(); // opencode not installed
    };
    if !out.status.success() {
        tracing::debug!(
            "opencode db failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return Vec::new();
    }
    let Ok(rows) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
        return Vec::new();
    };
    let project = normalize_dir(&project.to_string_lossy());
    let mut dead = Vec::new();
    for row in rows {
        let dir = row.get("dir").and_then(|d| d.as_str()).unwrap_or("");
        if normalize_dir(dir) != project {
            continue;
        }
        let Some(data) = row
            .get("data")
            .and_then(|d| d.as_str())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        else {
            continue;
        };
        if let Some(error_text) = opencode_quota_text(&data) {
            dead.push(Dead {
                agent: Agent::Opencode,
                session_id: row
                    .get("sid")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                path: None,
                error_id: row
                    .get("mid")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                error_text,
            });
        }
    }
    dead
}

/// Directory strings comparable across sources: strip Windows' verbatim
/// `\\?\` prefix (canonicalize adds it; opencode doesn't) and trailing separators.
fn normalize_dir(s: &str) -> String {
    s.strip_prefix(r"\\?\")
        .unwrap_or(s)
        .trim_end_matches(['/', '\\'])
        .to_string()
}

/// Limit-shaped error on an opencode assistant message, if any.
/// Shape: `{"role":"assistant",...,"error":{"name":"APIError","data":{"message":"..."}}}`.
fn opencode_quota_text(data: &serde_json::Value) -> Option<String> {
    if data.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let err = data.get("error")?;
    let name = err.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let msg = err
        .pointer("/data/message")
        .or_else(|| err.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    let hay = format!("{name} {msg}").to_ascii_lowercase();
    // ponytail: keyword match, not per-provider error taxonomy — widen if a
    // provider's quota error slips through.
    (hay.contains("limit") || hay.contains("quota") || hay.contains("429"))
        .then(|| if msg.is_empty() { name.to_string() } else { msg.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota_line(uuid: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "isApiErrorMessage": true,
            "message": {
                "role": "assistant",
                "model": "<synthetic>",
                "content": [{"type": "text", "text": "You've hit your session limit · resets 5:50pm (America/Toronto)"}]
            }
        })
        .to_string()
    }

    fn normal_line() -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": "ok-1",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done!"}]}
        })
        .to_string()
    }

    #[test]
    fn claude_dead_detects_trailing_quota_error() {
        let lines = vec![normal_line(), quota_line("err-1")];
        let (id, text) = claude_dead(&lines).expect("dead");
        assert_eq!(id, "err-1");
        assert!(text.contains("session limit"));
    }

    #[test]
    fn claude_alive_when_newest_assistant_is_normal() {
        // recovered: quota error followed by a normal assistant message
        let lines = vec![quota_line("err-1"), normal_line()];
        assert!(claude_dead(&lines).is_none());
    }

    #[test]
    fn claude_ignores_non_limit_api_errors() {
        let line = serde_json::json!({
            "type": "assistant",
            "uuid": "auth-1",
            "isApiErrorMessage": true,
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Please run /login · API Error: 401 Invalid authentication credentials"}]}
        })
        .to_string();
        assert!(claude_dead(&[line]).is_none());
    }

    #[test]
    fn scan_claude_dir_finds_dead_transcript() {
        let dir = std::env::temp_dir().join(format!("baton-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("dead.jsonl"),
            format!("{}\n{}\n", normal_line(), quota_line("err-9")),
        )
        .unwrap();
        std::fs::write(dir.join("alive.jsonl"), format!("{}\n", normal_line())).unwrap();

        let dead = scan_claude_dir(&dir);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].session_id, "dead");
        assert_eq!(dead[0].error_id, "err-9");
    }

    #[test]
    fn opencode_quota_matches_limit_errors_only() {
        let quota = serde_json::json!({
            "role": "assistant",
            "error": {"name": "APIError", "data": {"message": "Anthropic usage limit reached for this billing period"}}
        });
        assert!(opencode_quota_text(&quota).unwrap().contains("usage limit"));

        let rate = serde_json::json!({
            "role": "assistant",
            "error": {"name": "RateLimitError", "data": {}}
        });
        assert_eq!(opencode_quota_text(&rate).unwrap(), "RateLimitError");

        let auth = serde_json::json!({
            "role": "assistant",
            "error": {"name": "APIError", "data": {"message": "Incorrect API key provided: sk-or-v1***"}}
        });
        assert!(opencode_quota_text(&auth).is_none());

        let user = serde_json::json!({"role": "user"});
        assert!(opencode_quota_text(&user).is_none());
    }

    #[test]
    fn state_round_trip_and_dedup_key() {
        let dir = std::env::temp_dir().join(format!("baton-state-test-{}", std::process::id()));
        let path = dir.join("failover.json");
        let mut s = State::default();
        s.opt_in.push(State::key(Agent::ClaudeCode, "abc"));
        s.handled
            .insert(State::key(Agent::Opencode, "ses_1"), "msg_9".into());
        s.save_to(&path).unwrap();
        let back = State::load_from(&path);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.opt_in, vec!["claude-code:abc"]);
        assert_eq!(back.handled.get("opencode:ses_1").map(String::as_str), Some("msg_9"));
    }

    #[test]
    fn normalize_dir_strips_verbatim_prefix_and_trailing_seps() {
        assert_eq!(normalize_dir(r"\\?\C:\work\proj\"), r"C:\work\proj");
        assert_eq!(normalize_dir("/Users/x/proj/"), "/Users/x/proj");
        assert_eq!(normalize_dir("/Users/x/proj"), "/Users/x/proj");
    }

    #[test]
    fn tail_lines_drops_partial_first_line() {
        let dir = std::env::temp_dir().join(format!("baton-tail-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        std::fs::write(&p, "first-long-line\nsecond\nthird\n").unwrap();

        // 10 bytes from the end lands mid-"second" — that partial line must go
        let lines = tail_lines(&p, 10).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(lines, vec!["third"]);
    }
}
