//! Aider session format.
//!
//! Aider stores chat history per-repo in `.aider.chat.history.md` (Markdown) and also writes
//! a JSON-lines log at `.aider.chat.history.mdl` if logging is enabled. The Markdown is the
//! primary source. Messages are fenced with `#### USER:` / `#### ASSISTANT:` headers and tool
//! calls appear as fenced code blocks.
//!
//! TODO: implement a Markdown-parsing reader that maps user/assistant blocks to canonical parts.

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct Aider;

impl Format for Aider {
    const AGENT: Agent = Agent::Aider;
    const NAME: &'static str = "Aider";

    fn session_dir() -> PathBuf {
        // Aider is per-repo, so we look in the cwd's .aider.chat.history.md by default.
        PathBuf::from(".")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "aider read not implemented yet (file: {}). Aider stores chats as Markdown; see formats/aider.rs.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("aider write not implemented yet.")
    }
}
