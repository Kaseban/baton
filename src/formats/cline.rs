//! Cline / Roo Code session format.
//!
//! Cline (and its Roo Code fork) store task history in:
//!   `~/.vscode/extensions/.../cline/` or `~/.cursor/extensions/.../cline/`
//! and per-task conversation JSON at:
//!   `~/<workspace>/.cline/<task-id>/conversation.json`  (or `Global State` storage)
//!
//! Format: `{ "task": "...", "messages": [{ "ts":..., "type":"say"|"ask", "say":"user"|"assistant"|"tool", "text":"..." }] }`.
//!
//! TODO: implement reader + resolve the workspace-relative path discovery.

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct Cline;

impl Format for Cline {
    const AGENT: Agent = Agent::Cline;
    const NAME: &'static str = "Cline / Roo Code";

    fn session_dir() -> PathBuf {
        // Best-effort: Cline stores in the workspace under .cline/
        PathBuf::from(".cline")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "cline read not implemented yet (file: {}). See formats/cline.rs for the schema.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cline write not implemented yet.")
    }
}
