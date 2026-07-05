//! Zed AI assistant session format.
//!
//! Zed stores assistant panel conversations in its settings dir:
//!   macOS:  `~/Library/Application Support/Zed/assistant/conversations/*.json`
//!   Linux:  `~/.local/share/zed/assistant/conversations/*.json`
//!   Windows: `%APPDATA%\Zed\assistant\conversations\*.json`
//!
//! Format: array of message objects with `role` + `content` (text or tool calls).

use std::path::PathBuf;

use crate::canonical::{Agent, Format, Session};

pub struct Zed;

impl Format for Zed {
    const AGENT: Agent = Agent::Zed;
    const NAME: &'static str = "Zed";

    fn session_dir() -> PathBuf {
        if cfg!(target_os = "macos") {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Library")
                .join("Application Support")
                .join("Zed")
                .join("assistant")
                .join("conversations")
        } else if cfg!(target_os = "windows") {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Zed")
                .join("assistant")
                .join("conversations")
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("zed")
                .join("assistant")
                .join("conversations")
        }
    }

    fn read(path: &std::path::Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "zed read not implemented yet (file: {}). See formats/zed.rs for the schema.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("zed write not implemented yet.")
    }
}
