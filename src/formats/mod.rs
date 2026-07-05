//! Format codecs — each agent format implements [`crate::canonical::Format`].
//!
//! Adding a new format:
//! 1. Create `formats/<name>.rs` with a struct implementing `Format`.
//! 2. Register it in [`dispatch`] below.
//! 3. Read/write logic funnels through [`canonical::Session`].

pub mod claude_code;
pub mod opencode;
pub mod codex;
pub mod cursor;
pub mod continue_dev;
pub mod cline;
pub mod zed;
pub mod aider;
pub mod gemini_cli;

use std::path::Path;

use crate::canonical::{Agent, Format, Session, SessionRef};

/// Dispatch a read by agent.
pub fn read(agent: Agent, path: &Path) -> anyhow::Result<Session> {
    match agent {
        Agent::ClaudeCode => ClaudeCode::read(path),
        Agent::Opencode => Opencode::read(path),
        Agent::Codex => Codex::read(path),
        Agent::Cursor => Cursor::read(path),
        Agent::Continue => ContinueDev::read(path),
        Agent::Cline => Cline::read(path),
        Agent::Zed => Zed::read(path),
        Agent::Aider => Aider::read(path),
        Agent::GeminiCli => GeminiCli::read(path),
        Agent::Unknown => anyhow::bail!("unknown agent"),
    }
}

/// Dispatch a write by agent.
pub fn write(agent: Agent, session: &Session, path: &Path) -> anyhow::Result<()> {
    match agent {
        Agent::ClaudeCode => ClaudeCode::write(session, path),
        Agent::Opencode => Opencode::write(session, path),
        Agent::Codex => Codex::write(session, path),
        Agent::Cursor => Cursor::write(session, path),
        Agent::Continue => ContinueDev::write(session, path),
        Agent::Cline => Cline::write(session, path),
        Agent::Zed => Zed::write(session, path),
        Agent::Aider => Aider::write(session, path),
        Agent::GeminiCli => GeminiCli::write(session, path),
        Agent::Unknown => anyhow::bail!("unknown agent"),
    }
}

/// Dispatch list by agent.
pub fn list(agent: Agent) -> Vec<SessionRef> {
    match agent {
        Agent::ClaudeCode => ClaudeCode::list(),
        Agent::Opencode => Opencode::list(),
        Agent::Codex => Codex::list(),
        Agent::Cursor => Cursor::list(),
        Agent::Continue => ContinueDev::list(),
        Agent::Cline => Cline::list(),
        Agent::Zed => Zed::list(),
        Agent::Aider => Aider::list(),
        Agent::GeminiCli => GeminiCli::list(),
        Agent::Unknown => Vec::new(),
    }
}

/// All agents we support, in display order.
pub const ALL_AGENTS: &[Agent] = &[
    Agent::ClaudeCode,
    Agent::Opencode,
    Agent::Codex,
    Agent::Cursor,
    Agent::Continue,
    Agent::Cline,
    Agent::Zed,
    Agent::Aider,
    Agent::GeminiCli,
];

use claude_code::ClaudeCode;
use cline::Cline;
use codex::Codex;
use continue_dev::ContinueDev;
use cursor::Cursor;
use gemini_cli::GeminiCli;
use aider::Aider;
use opencode::Opencode;
use zed::Zed;
