//! Continue.dev session format.
//!
//! Continue stores sessions in `~/.continue/sessions/<uuid>.json` (one file per session).
//! Each file: `{ "sessionId": "...", "workspace": {...}, "messages": [{ "role": "user"|"assistant", "content": "..." }] }`.
//! Content may be a string or an array of content blocks (text / tool call / image).

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct ContinueDev;

impl Format for ContinueDev {
    const AGENT: Agent = Agent::Continue;
    const NAME: &'static str = "Continue";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".continue")
            .join("sessions")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "continue read not implemented yet (file: {}). See formats/continue_dev.rs for the schema.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("continue write not implemented yet.")
    }
}
