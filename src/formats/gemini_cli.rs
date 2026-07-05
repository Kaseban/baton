//! Gemini CLI session format.
//!
//! Gemini CLI stores sessions under `~/.gemini/tmp/<session-id>/` with chunked transcript
//! JSON files, or in `~/.gemini/` as `*.json`. The transcript schema mirrors the
//! Google Generative AI `GenerateContentResponse` shape (candidates / parts / functionCall).
//!
//! TODO: implement reader once the on-disk shape is confirmed across Gemini CLI versions.

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct GeminiCli;

impl Format for GeminiCli {
    const AGENT: Agent = Agent::GeminiCli;
    const NAME: &'static str = "Gemini CLI";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".gemini")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "gemini-cli read not implemented yet (file: {}). See formats/gemini_cli.rs for the format spec.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("gemini-cli write not implemented yet.")
    }
}
