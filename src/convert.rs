//! Convert a session from one agent format to another.

use std::path::Path;

use anyhow::Context;

use crate::canonical::{Agent, Session};
use crate::formats;

/// Returns the path the converted session was written to.
pub fn convert(
    from: Agent,
    to: Agent,
    input: &Path,
    output: Option<&Path>,
    compress: bool,
) -> anyhow::Result<std::path::PathBuf> {
    let mut session = formats::read(from, input)
        .with_context(|| format!("reading {} session", from))?;
    if compress {
        let before = session.message_count();
        session.compress();
        eprintln!(
            "compressed: stripped tool calls/outputs ({} → {} messages)",
            before,
            session.message_count()
        );
    }
    let out = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_output(&session, to));
    formats::write(to, &session, &out)
        .with_context(|| format!("writing {} session", to))?;
    eprintln!(
        "passed baton: {} → {} ({} messages) → {}",
        from,
        to,
        session.message_count(),
        out.display()
    );
    Ok(out)
}

fn default_output(session: &Session, to: Agent) -> std::path::PathBuf {
    let ext = match to {
        Agent::ClaudeCode => "jsonl",
        Agent::Opencode => "json",
        _ => "json",
    };
    let stem = &session.source_id;
    std::env::current_dir()
        .unwrap_or_default()
        .join(format!("{}-{}.{}", to, stem, ext))
}

/// Run the target agent's own import command (if supported) after writing the converted file.
pub fn import_to_target(to: Agent, file: &Path) -> anyhow::Result<()> {
    match to {
        Agent::Opencode => {
            use std::io::Write;
            let out = std::process::Command::new("opencode")
                .arg("import")
                .arg(file)
                .output()
                .context("running `opencode import`")?;
            std::io::stdout().write_all(&out.stdout).ok();
            std::io::stderr().write_all(&out.stderr).ok();
            if !out.status.success() {
                anyhow::bail!("`opencode import` exited {}", out.status);
            }
            // opencode prints "Imported session: ses_..." — lift the id so we can
            // hand the user a ready-to-run command instead of making them hunt.
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let id = combined
                .split_whitespace()
                .find(|w| w.starts_with("ses_"))
                .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()));
            match id {
                Some(id) => eprintln!("open it with: opencode --session {id}"),
                None => eprintln!("open it with: opencode --continue"),
            }
            Ok(())
        }
        Agent::ClaudeCode => import_claude(file),
        Agent::Codex => import_codex(file),
        other => anyhow::bail!("no auto-import path for {other} yet"),
    }
}

/// Claude encodes a project directory by replacing path separators (and dots) with dashes.
pub fn encode_claude_project_dir(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == '.' || c == ':' { '-' } else { c })
        .collect()
}

/// Place a converted .jsonl where Claude Code will find it:
/// `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`, resumable via `claude --resume <uuid>`.
fn import_claude(file: &Path) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?;

    // Session id must be a UUID (it becomes the resume id). Reuse the one in the
    // file if valid, else mint one and rewrite the sessionId fields to match.
    let existing_id = raw
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|v| v.get("sessionId").and_then(|s| s.as_str()).map(String::from));
    let (id, contents) = match existing_id.filter(|id| uuid::Uuid::parse_str(id).is_ok()) {
        Some(id) => (id, raw),
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let rewritten: Vec<String> = raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| match serde_json::from_str::<serde_json::Value>(l) {
                    Ok(mut v) => {
                        v["sessionId"] = serde_json::json!(id);
                        v.to_string()
                    }
                    Err(_) => l.to_string(),
                })
                .collect();
            (id, rewritten.join("\n") + "\n")
        }
    };

    let cwd = std::env::current_dir().context("getting cwd")?;
    let encoded = encode_claude_project_dir(&cwd);
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".claude")
        .join("projects")
        .join(encoded);
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let dest = dir.join(format!("{id}.jsonl"));
    std::fs::write(&dest, contents).with_context(|| format!("writing {}", dest.display()))?;
    eprintln!("imported into {}", dest.display());
    eprintln!(
        "open it with: claude --resume {id}   (from {})",
        cwd.display()
    );
    Ok(())
}

/// Place a converted rollout where codex will find it:
/// `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`, then `codex resume <uuid>`.
fn import_codex(file: &Path) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?;
    // The session UUID lives in the first session_meta line (the writer always emits one).
    let id = raw
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("session_meta"))
        .and_then(|v| {
            v.get("payload")
                .and_then(|p| p.get("id"))
                .and_then(|i| i.as_str())
                .map(String::from)
        })
        .context("no session_meta line with a session id — was this file written by baton?")?;

    let now = chrono::Utc::now();
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".codex")
        .join("sessions")
        .join(now.format("%Y").to_string())
        .join(now.format("%m").to_string())
        .join(now.format("%d").to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let dest = dir.join(format!(
        "rollout-{}-{id}.jsonl",
        now.format("%Y-%m-%dT%H-%M-%S")
    ));
    std::fs::copy(file, &dest)
        .with_context(|| format!("copying to {}", dest.display()))?;
    eprintln!("imported into {}", dest.display());
    eprintln!("open it with: codex resume {id}");
    Ok(())
}
