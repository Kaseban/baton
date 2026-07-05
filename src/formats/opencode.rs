//! OpenCode session format.
//!
//! OpenCode stores sessions in SQLite but exposes an import/export JSON shape:
//!   {
//!     "info": { SessionV1.Info },
//!     "messages": [{ "info": MessageV1.Info, "parts": [Part] }]
//!   }
//!
//! `opencode import <file>` validates via effect Schema (SessionV1.Info / SessionV1.Part).
//! Key required fields discovered from source (`packages/opencode/src/cli/cmd/import.ts`):
//!   - message info must include `sessionID`
//!   - part must include `sessionID` + `messageID`
//!   - session info needs `id`, `slug`, `projectID`, `directory`, `title`, `agent`, `model`,
//!     `version`, `summary`, `cost`, `tokens`, `time.created`/`time.updated`

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use uuid::Uuid;

use crate::canonical::{Agent, Format, Message, Part, Role, Session};

pub struct Opencode;

impl Format for Opencode {
    const AGENT: Agent = Agent::Opencode;
    const NAME: &'static str = "OpenCode";

    fn session_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opencode")
            .join("storage")
    }

    fn read(path: &Path) -> anyhow::Result<Session> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading opencode session {}", path.display()))?;
        let export: ExportData = serde_json::from_str(&raw)?;

        let info = &export.info;
        let mut messages = Vec::new();
        for msg in &export.messages {
            let role = match msg.info.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };
            let ts = msg
                .info
                .time
                .created
                .unwrap_or(info.time.created.unwrap_or(0));
            let parts: Vec<Part> = msg
                .parts
                .iter()
                .filter_map(|p| match p.part_type.as_str() {
                    "text" => p.text.as_ref().map(|t| Part::Text { text: t.clone() }),
                    "reasoning" | "reasoning.text" => {
                        p.text.as_ref().map(|t| Part::Reasoning { text: t.clone() })
                    }
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                continue;
            }
            messages.push(Message {
                role,
                parts,
                time_created: ts,
                origin: Some(Agent::Opencode),
            });
        }

        Ok(Session {
            source_id: info.id.clone(),
            origin: Agent::Opencode,
            title: info.title.clone(),
            time_created: info.time.created.unwrap_or(0),
            time_updated: info.time.updated.unwrap_or(0),
            directory: Some(info.directory.clone()),
            messages,
        })
    }

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        let default_dir = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| ".".to_string());
        let project_id = compute_project_id(
            session.directory.as_deref().unwrap_or(&default_dir),
        );
        let now = chrono::Utc::now().timestamp_millis();
        let session_id = format!("ses_{}", &Uuid::new_v4().simple().to_string()[..24]);
        let slug = slugify(&session.title);

        let mut out_messages = Vec::with_capacity(session.messages.len());
        let mut prev_id: Option<String> = None;
        let cwd = session
            .directory
            .clone()
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|p| p.to_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| ".".to_string())
            });
        for msg in &session.messages {
            let msg_id = format!("msg_{}", &Uuid::new_v4().simple().to_string()[..24]);
            let ts = msg.time_created;
            let msg_info: serde_json::Value = match msg.role {
                Role::User => serde_json::json!({
                    "id": msg_id,
                    "sessionID": session_id,
                    "role": "user",
                    "time": { "created": ts },
                    "agent": "build",
                    "model": { "providerID": "openrouter", "modelID": "z-ai/glm-5.2" },
                }),
                Role::Assistant => {
                    let pid = prev_id.clone().unwrap_or_else(|| msg_id.clone());
                    serde_json::json!({
                        "id": msg_id,
                        "sessionID": session_id,
                        "role": "assistant",
                        "time": { "created": ts, "completed": ts + 1 },
                        "parentID": pid,
                        "modelID": "z-ai/glm-5.2",
                        "providerID": "openrouter",
                        "mode": "build",
                        "agent": "build",
                        "path": { "cwd": cwd, "root": cwd },
                        "cost": 0,
                        "tokens": {
                            "input": 0,
                            "output": 0,
                            "reasoning": 0,
                            "cache": { "read": 0, "write": 0 }
                        },
                    })
                }
                Role::System => serde_json::json!({
                    "id": msg_id,
                    "sessionID": session_id,
                    "role": "user",
                    "time": { "created": ts },
                    "agent": "build",
                    "model": { "providerID": "openrouter", "modelID": "z-ai/glm-5.2" },
                }),
            };

            let parts_json: Vec<serde_json::Value> = msg
                .parts
                .iter()
                .map(|p| {
                    let part_id = format!("prt_{}", &Uuid::new_v4().simple().to_string()[..24]);
                    match p {
                        Part::Text { text } => serde_json::json!({
                            "type": "text",
                            "text": text,
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                        }),
                        Part::Reasoning { text } => serde_json::json!({
                            "type": "reasoning",
                            "text": text,
                            "time": { "start": ts, "end": ts + 1 },
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                        }),
                        Part::ToolCall { name, input } => serde_json::json!({
                            "type": "step-start",
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                            "_tool": name,
                            "_input": input.clone().unwrap_or(serde_json::Value::Null),
                        }),
                        Part::ToolResult { name, output, is_error } => serde_json::json!({
                            "type": "text",
                            "text": format!("[tool result: {}] {}", name, output.clone().unwrap_or_default()),
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                        }),
                        Part::Attachment { mime, path, data } => {
                            let _ = (mime, path, data);
                            serde_json::json!({
                                "type": "text",
                                "text": "[attachment]",
                                "id": part_id,
                                "sessionID": session_id,
                                "messageID": msg_id,
                            })
                        }
                    }
                })
                .collect();

            out_messages.push(serde_json::json!({
                "info": msg_info,
                "parts": parts_json,
            }));
            prev_id = Some(msg_id);
        }

        let export = serde_json::json!({
            "info": {
                "id": session_id,
                "slug": slug,
                "projectID": project_id,
                "directory": session.directory.clone().unwrap_or_else(|| ".".to_string()),
                "path": "",
                "title": format!("[{}] {}", session.origin, session.title),
                "agent": "build",
                "model": { "id": "z-ai/glm-5.2", "providerID": "openrouter" },
                "version": env!("CARGO_PKG_VERSION"),
                "summary": { "additions": 0, "deletions": 0, "files": 0 },
                "cost": 0,
                "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": {
                    "created": if session.time_created > 0 { session.time_created } else { now },
                    "updated": if session.time_updated > 0 { session.time_updated } else { now },
                },
            },
            "messages": out_messages,
        });

        let pretty = serde_json::to_string_pretty(&export)?;
        std::fs::write(out_path, pretty)
            .with_context(|| format!("writing {}", out_path.display()))?;
        Ok(())
    }
}

fn slugify(s: &str) -> String {
    let s = s
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let s = s.trim_matches('-').to_string();
    let s: String = s.chars().take(40).collect();
    if s.is_empty() {
        "imported".to_string()
    } else {
        s
    }
}

fn compute_project_id(dir: &str) -> String {
    let abs = std::fs::canonicalize(dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| dir.to_string());
    let mut hasher = Sha1::new();
    hasher.update(abs.as_bytes());
    hex_encode(&hasher.finalize())
}

// minimal sha1 (avoid pulling a crate for one hash)
struct Sha1 {
    state: [u32; 5],
    len: u64,
    buf: Vec<u8>,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            len: 0,
            buf: Vec::with_capacity(64),
        }
    }
    fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u64;
        self.buf.extend_from_slice(data);
        while self.buf.len() >= 64 {
            let block: [u8; 64] = self.buf[..64].try_into().unwrap();
            self.process_block(&block);
            self.buf.drain(..64);
        }
    }
    fn finalize(mut self) -> [u8; 20] {
        let bit_len = self.len * 8;
        self.buf.push(0x80);
        while self.buf.len() % 64 != 56 {
            self.buf.push(0);
        }
        self.buf.extend_from_slice(&bit_len.to_be_bytes());
        while self.buf.len() >= 64 {
            let block: [u8; 64] = self.buf[..64].try_into().unwrap();
            self.process_block(&block);
            self.buf.drain(..64);
        }
        let mut out = [0u8; 20];
        for (i, &s) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }
    fn process_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// --- deserialization types for reading opencode exports ---

#[derive(Debug, serde::Deserialize)]
struct ExportData {
    info: SessionInfo,
    #[serde(default)]
    messages: Vec<ExportMessage>,
}

#[derive(Debug, serde::Deserialize)]
struct SessionInfo {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    directory: String,
    #[serde(default)]
    time: TimeField,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TimeField {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    updated: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct ExportMessage {
    info: MessageInfo,
    #[serde(default)]
    parts: Vec<ExportPart>,
}

#[derive(Debug, serde::Deserialize)]
struct MessageInfo {
    #[serde(default)]
    role: String,
    #[serde(default)]
    time: TimeField,
}

#[derive(Debug, serde::Deserialize)]
struct ExportPart {
    #[serde(rename = "type", default)]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Serialize)]
struct _Phantom;
