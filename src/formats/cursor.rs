//! Cursor AI session format.
//!
//! Cursor stores chat history in `~/.cursor/` (state.vscdb SQLite) and per-workspace
//! `workspaceStorage/` IndexedDB. The chat table in `state.vscdb` holds serialized messages.
//! There is no documented resume-from-JSON path; conversion is one-way OUT for archival
//! or INTO opencode/claude.
//!
//! TODO: implement reader by querying the SQLite state.vscdb `ItemTable` for `aiService:chat`.

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct Cursor;

impl Format for Cursor {
    const AGENT: Agent = Agent::Cursor;
    const NAME: &'static str = "Cursor";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".cursor")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "cursor read not implemented yet (file: {}). Cursor stores chats in state.vscdb SQLite; see formats/cursor.rs.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cursor write not implemented. Cursor has no JSON import path.")
    }
}
