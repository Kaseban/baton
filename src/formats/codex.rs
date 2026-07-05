//! OpenAI Codex CLI session format.
//!
//! Codex stores sessions under `~/.codex/sessions/<date>/<session-id>.jsonl` (rollout format).
//! Each line is a JSON event with a `type` field: `message`, `function_call`, `function_call_output`,
//! `reasoning`, etc. The `message` events carry `role` + `content` arrays similar to OpenAI's
//! Responses API.
//!
//! TODO: implement reader. Writer is low priority (Codex resumes via its own session dir, no import CLI).

use std::path::{Path, PathBuf};

use crate::canonical::{Agent, Format, Session};

pub struct Codex;

impl Format for Codex {
    const AGENT: Agent = Agent::Codex;
    const NAME: &'static str = "Codex CLI";

    fn session_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex")
            .join("sessions")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        anyhow::bail!(
            "codex read not implemented yet (file: {}). See formats/codex.rs for the format spec.",
            path.display()
        )
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("codex write not implemented yet. Codex resumes via ~/.codex/sessions/ directly.")
    }
}
