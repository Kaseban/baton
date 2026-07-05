//! Convert a session from one agent format to another.

use std::path::Path;

use anyhow::Context;

use crate::canonical::{Agent, Session};
use crate::formats;

pub fn convert(
    from: Agent,
    to: Agent,
    input: &Path,
    output: Option<&Path>,
) -> anyhow::Result<()> {
    let session = formats::read(from, input)
        .with_context(|| format!("reading {} session", from))?;
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
    Ok(())
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
            let status = std::process::Command::new("opencode")
                .arg("import")
                .arg(file)
                .status()
                .context("running `opencode import`")?;
            if !status.success() {
                anyhow::bail!("`opencode import` exited {status}");
            }
            Ok(())
        }
        Agent::ClaudeCode => {
            // Claude Code resumes from ~/.claude/projects/<path>/<id>.jsonl directly.
            anyhow::bail!(
                "claude-code resumes sessions by placing the .jsonl into ~/.claude/projects/<encoded-path>/<id>.jsonl; auto-import not wired yet."
            )
        }
        other => anyhow::bail!("no auto-import path for {other} yet"),
    }
}
