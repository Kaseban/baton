//! Claude Desktop's macOS file-level Code-tab registration (observed in 2.9939.2).

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use uuid::Uuid;

use crate::canonical::Session;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration<'a> {
    session_id: String,
    cli_session_id: &'a str,
    cwd: &'a Path,
    origin_cwd: &'a Path,
    created_at: i64,
    last_activity_at: i64,
    last_focused_at: i64,
    model: &'static str,
    effort: &'static str,
    is_archived: bool,
    title: &'a str,
    title_source: &'static str,
    permission_mode: &'static str,
    chrome_permission_mode: &'static str,
    completed_turns: usize,
    classifier_summary_enabled: bool,
    report_findings_card: bool,
    spawn_seed: BTreeMap<String, String>,
    always_allowed_reasons: [String; 0],
    session_permission_updates: [String; 0],
    remote_mcp_servers_config: [String; 0],
    steered_by_remote_client: Option<bool>,
}

impl<'a> Registration<'a> {
    pub fn new(id: &'a str, cwd: &'a Path, session: &'a Session) -> anyhow::Result<Self> {
        Uuid::parse_str(id).context("invalid imported Claude Code session ID")?;
        Ok(Self {
            session_id: format!("local_{}", Uuid::new_v4()),
            cli_session_id: id,
            cwd,
            origin_cwd: cwd,
            created_at: session.time_created,
            last_activity_at: session.time_updated,
            last_focused_at: chrono::Utc::now().timestamp_millis(),
            model: "default",
            effort: "high",
            is_archived: false,
            title: &session.title,
            title_source: "auto",
            permission_mode: "default",
            chrome_permission_mode: "ask",
            completed_turns: 0,
            classifier_summary_enabled: false,
            report_findings_card: false,
            spawn_seed: BTreeMap::new(),
            always_allowed_reasons: [],
            session_permission_updates: [],
            remote_mcp_servers_config: [],
            steered_by_remote_client: None,
        })
    }
}

pub fn account_scopes(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    let mut scopes = Vec::new();
    for account in uuid_directories(root)
        .context("finding Claude Desktop accounts; open Desktop and sign in first")?
    {
        scopes.extend(uuid_directories(&account)?);
    }
    Ok(scopes)
}

fn uuid_directories(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    anyhow::ensure!(
        fs::symlink_metadata(root)?.file_type().is_dir(),
        "Desktop directory must not be a symlink: {}",
        root.display()
    );
    let mut paths = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| Uuid::parse_str(name).is_ok())
        {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

pub fn register(scopes: &[PathBuf], entry: &Registration<'_>) -> anyhow::Result<usize> {
    let bytes = serde_json::to_vec_pretty(entry)?;
    let mut created = 0;
    for scope in scopes {
        let path = scope.join(format!("{}.json", entry.session_id));
        let mut file = tempfile::NamedTempFile::new_in(scope)
            .with_context(|| format!("creating registration in {}", scope.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        // Publish a complete 0600 file without replacing an existing file or symlink.
        match file.persist_noclobber(&path) {
            Ok(_) => {
                created += 1;
            }
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "creating {}; CLI transcript is already imported",
                        path.display()
                    )
                });
            }
        }
    }
    Ok(created)
}

#[cfg(test)]
#[path = "desktop_tests.rs"]
mod tests;
