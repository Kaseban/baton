//! Finding a Codex thread's rollout files and loading its records.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;

use crate::canonical::walk_session_files;

/// One line of a rollout.
#[derive(Debug, Clone)]
pub(super) struct Record {
    /// Epoch milliseconds, 0 when the line has no timestamp.
    pub ts: i64,
    pub ordinal: Option<i64>,
    /// Envelope type; a bare legacy ResponseItem line reads as `response_item`.
    pub kind: String,
    pub payload: Value,
}

/// What a thread's `session_meta` records about it.
#[derive(Debug, Clone, Default)]
pub(super) struct Meta {
    pub id: String,
    pub timestamp: i64,
    pub cwd: Option<String>,
    pub originator: Option<String>,
    /// `cli`, `exec`, `vscode`, `mcp`, ... or `subagent`.
    pub source: Option<String>,
    /// For a subagent: `thread_spawn`, or the kind Codex names (`review`, ...).
    pub subagent: Option<String>,
    pub thread_source: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub forked_from_id: Option<String>,
    /// A subagent's own records start at this ordinal; the ones before it are
    /// a copy of its parent's history.
    pub history_start: Option<i64>,
    /// A fork's history is the base thread's records before this ordinal.
    pub history_base: Option<(String, i64)>,
}

impl Meta {
    fn parse(p: &Value) -> Self {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let (source, subagent, spawn) = match p.get("source") {
            Some(Value::String(src)) => (Some(src.clone()), None, Value::Null),
            Some(Value::Object(o)) => match o.get("subagent") {
                Some(Value::String(kind)) => (Some("subagent".into()), Some(kind.clone()), Value::Null),
                Some(Value::Object(sub)) => {
                    let kind = sub.keys().next().cloned();
                    let spawn = sub.get("thread_spawn").cloned().unwrap_or(Value::Null);
                    (Some("subagent".into()), kind, spawn)
                }
                _ => (o.keys().next().cloned(), None, Value::Null),
            },
            _ => (None, None, Value::Null),
        };
        let either = |k: &str| s(p, k).or_else(|| s(&spawn, k));
        Meta {
            id: s(p, "id").unwrap_or_default(),
            timestamp: p.get("timestamp").and_then(Value::as_str).map(parse_ts).unwrap_or(0),
            cwd: s(p, "cwd"),
            originator: s(p, "originator"),
            source,
            subagent,
            thread_source: s(p, "thread_source"),
            cli_version: s(p, "cli_version"),
            model_provider: s(p, "model_provider"),
            parent_thread_id: either("parent_thread_id"),
            agent_path: either("agent_path"),
            agent_nickname: either("agent_nickname"),
            agent_role: either("agent_role"),
            forked_from_id: s(p, "forked_from_id"),
            history_start: p.get("subagent_history_start_ordinal").and_then(Value::as_i64),
            history_base: p.get("history_base").and_then(|b| {
                Some((s(b, "thread_id")?, b.get("end_ordinal_exclusive")?.as_i64()?))
            }),
        }
    }
}

pub(super) fn parse_ts(t: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(t).map(|dt| dt.timestamp_millis()).unwrap_or(0)
}

fn parse_line(line: &str) -> Option<Record> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let ts = v.get("timestamp").and_then(Value::as_str).map(parse_ts).unwrap_or(0);
    let ordinal = v.get("ordinal").and_then(Value::as_i64);
    match (v.get("type").and_then(Value::as_str), v.get("payload")) {
        (Some(kind), Some(payload)) => {
            Some(Record { ts, ordinal, kind: kind.to_string(), payload: payload.clone() })
        }
        // Legacy rollouts: a bare ResponseItem per line, or the old header line.
        (Some(_), None) => Some(Record { ts, ordinal, kind: "response_item".into(), payload: v }),
        (None, _) if v.get("id").is_some() => {
            Some(Record { ts, ordinal, kind: "session_meta".into(), payload: v })
        }
        _ => None,
    }
}

pub(super) fn read_file(path: &Path) -> anyhow::Result<Vec<Record>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading codex rollout {}", path.display()))?;
    Ok(raw.lines().filter(|l| !l.trim().is_empty()).filter_map(parse_line).collect())
}

/// The rollouts under one Codex home (`~/.codex`).
pub(super) struct Home {
    pub root: Option<PathBuf>,
    files: Vec<PathBuf>,
}

impl Home {
    /// The Codex home a rollout lives in: the parent of the `sessions` or
    /// `archived_sessions` directory above it.
    pub fn around(path: &Path) -> Self {
        let root = std::path::absolute(path).ok().and_then(|abs| {
            abs.ancestors()
                .find(|a| matches!(a.file_name().and_then(|n| n.to_str()), Some("sessions" | "archived_sessions")))
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
        let mut files = Vec::new();
        if let Some(root) = &root {
            walk_session_files(&root.join("sessions"), 3, &mut |p, _| files.push(p.to_path_buf()));
            walk_session_files(&root.join("archived_sessions"), 0, &mut |p, _| files.push(p.to_path_buf()));
        }
        files.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
        Home { root, files }
    }

    /// Every rollout of thread `id`: `rollout-<ts>-<id>.jsonl` first, then its
    /// continuation windows `rollout-<ts>-<id>_<window>.jsonl` in name order.
    pub fn thread_files(&self, id: &str) -> Vec<PathBuf> {
        let primary = format!("-{id}.jsonl");
        let window = format!("-{id}_");
        let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut found: Vec<&PathBuf> = self
            .files
            .iter()
            .filter(|p| !id.is_empty() && (name(p).ends_with(&primary) || name(p).contains(&window)))
            .collect();
        found.sort_by_key(|p| (!name(p).ends_with(&primary), name(p)));
        found.into_iter().cloned().collect()
    }

    pub fn has_thread(&self, id: &str) -> bool {
        !self.thread_files(id).is_empty()
    }

    /// The thread's latest name in `session_index.jsonl`.
    pub fn thread_name(&self, id: &str) -> Option<String> {
        let raw = std::fs::read_to_string(self.root.as_ref()?.join("session_index.jsonl")).ok()?;
        raw.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["id"] == id)
            .filter_map(|v| {
                let name = v["thread_name"].as_str()?.trim().to_string();
                Some((v["updated_at"].as_str().unwrap_or_default().to_string(), name))
            })
            .filter(|(_, name)| !name.is_empty())
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, name)| name)
    }
}

/// Where a thread's rollouts are, and what its `session_meta` says.
pub(super) struct Located {
    pub meta: Meta,
    pub home: Home,
    /// Every rollout file of the thread, primary first.
    pub files: Vec<PathBuf>,
    pub archived: bool,
}

/// Find the thread `path` belongs to without reading its records.
pub(super) fn locate(path: &Path) -> anyhow::Result<Located> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).with_context(|| format!("reading codex rollout {}", path.display()))?;
    let mut lines = std::io::BufReader::new(file).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty());
    let Some(first) = lines.next() else {
        anyhow::bail!("empty rollout {}", path.display());
    };
    let meta = std::iter::once(first)
        .chain(lines.take(8))
        .filter_map(|l| parse_line(&l))
        .find(|r| r.kind == "session_meta")
        .map(|r| Meta::parse(&r.payload));
    let mut meta = meta.unwrap_or_default();
    if meta.id.is_empty() {
        meta.id = id_from_name(path);
    }
    let home = Home::around(path);
    let mut files = home.thread_files(&meta.id);
    let same = |a: &Path, b: &Path| std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok();
    if !files.iter().any(|f| same(f, path)) {
        files = vec![path.to_path_buf()];
    }
    let archived = files.iter().any(|f| f.components().any(|c| c.as_os_str() == "archived_sessions"));
    Ok(Located { meta, home, files, archived })
}

/// A thread with every record it is made of, in order.
pub(super) struct Thread {
    pub located: Located,
    pub records: Vec<Record>,
}

/// Load the thread `path` belongs to, from all of its rollout files.
pub(super) fn load(path: &Path) -> anyhow::Result<Thread> {
    let located = locate(path)?;
    let records = thread_records(&located.home, &located.files, &located.meta, 0)?;
    Ok(Thread { located, records })
}

/// A thread's records with the copied parent history before
/// `subagent_history_start_ordinal` dropped, and a fork's base history
/// prepended. `session_meta` records are left out.
fn thread_records(home: &Home, files: &[PathBuf], meta: &Meta, depth: u32) -> anyhow::Result<Vec<Record>> {
    let mut out = Vec::new();
    if let Some((base, end)) = &meta.history_base
        && depth < 8
    {
        let base_files = home.thread_files(base);
        if let Some(first) = base_files.first() {
            let base_meta = read_file(first)?
                .iter()
                .find(|r| r.kind == "session_meta")
                .map(|r| Meta::parse(&r.payload))
                .unwrap_or_default();
            // Windows restart their ordinals, so the base history runs up to the
            // first record at the fork point rather than every record below it.
            out.extend(
                thread_records(home, &base_files, &base_meta, depth + 1)?
                    .into_iter()
                    .take_while(|r| r.ordinal.is_none_or(|o| o < *end)),
            );
        }
    }
    for file in files {
        let records = read_file(file)?;
        let start = records
            .iter()
            .find(|r| r.kind == "session_meta")
            .and_then(|r| Meta::parse(&r.payload).history_start);
        out.extend(records.into_iter().filter(|r| {
            r.kind != "session_meta" && !matches!((start, r.ordinal), (Some(s), Some(o)) if o < s)
        }));
    }
    Ok(out)
}

fn id_from_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    let stem = stem.split('_').next().unwrap_or(stem);
    if stem.len() >= 36 && uuid::Uuid::parse_str(&stem[stem.len() - 36..]).is_ok() {
        stem[stem.len() - 36..].to_string()
    } else {
        stem.to_string()
    }
}
