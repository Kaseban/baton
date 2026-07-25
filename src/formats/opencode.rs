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
            let mut role = match msg.info.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            };
            let ts = msg
                .info
                .time
                .created
                .unwrap_or(info.time.created.unwrap_or(0));
            let mut parts: Vec<Part> = Vec::new();
            for p in &msg.parts {
                match p.part_type.as_str() {
                    "text" => {
                        if let Some(t) = &p.text {
                            parts.push(Part::Text { text: t.clone() });
                        }
                    }
                    "reasoning" | "reasoning.text" => {
                        if let Some(t) = &p.text {
                            parts.push(Part::Reasoning {
                                text: t.clone(),
                                signature: p.reasoning_signature(),
                            });
                        }
                    }
                    "tool" => {
                        let name = p.tool.clone().unwrap_or_else(|| "tool".to_string());
                        let id = p.call_id.clone();
                        let state = p.state.as_ref();
                        let input = state.and_then(|s| s.input.clone());
                        parts.push(Part::ToolCall {
                            name: name.clone(),
                            id: id.clone(),
                            input,
                        });
                        if let Some(s) = state {
                            let is_error = s.status.as_deref() == Some("error");
                            if s.output.is_some() || is_error {
                                parts.push(Part::ToolResult {
                                    name,
                                    id,
                                    output: s.output.clone(),
                                    is_error: Some(is_error),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            if parts.is_empty() {
                continue;
            }
            // baton writes system messages as user text prefixed "[system] " (opencode
            // only accepts user/assistant roles) — recover the original role here.
            if role == Role::User
                && let Some(Part::Text { text }) = parts.first_mut()
                    && let Some(rest) = text.strip_prefix("[system] ") {
                        *text = rest.to_string();
                        role = Role::System;
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
        // Identifiers must be TIME-SORTABLE, not random: opencode's run loop decides
        // whether a turn is finished with a raw lexicographic comparison of message
        // ids (`lastUser.id < lastAssistant.id`, packages/opencode/src/session/
        // prompt.ts), and `MessageV2.latest()` picks the "last" message the same way.
        // Random uuids break that ordering, so an imported message can outrank the
        // live ones, the loop never exits, and opencode re-requests with the
        // conversation ending on an assistant message -> Anthropic 400
        // "does not support assistant message prefill".
        let mut ids = IdGen::new();
        let session_id = ids.next("ses", session.time_created.max(0));
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
        // callID → (out_messages index, parts index) of the emitted "tool" part, so a
        // ToolResult arriving in a later message folds into that part's state.
        let mut tool_locs: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        // Rough usage estimates (~4 chars/token): opencode's UI expects real
        // numbers here, and all-zero usage on every message reads as a session
        // that never produced anything.
        let mut context_tokens: u64 = 0;
        let mut total_output: u64 = 0;
        for msg in &session.messages {
            let msg_id = ids.next("msg", msg.time_created.max(0));
            let ts = msg.time_created;
            let msg_info: serde_json::Value = match msg.role {
                // opencode only accepts user/assistant; system messages are written as
                // user with a "[system] " text prefix (recovered on read).
                Role::User | Role::System => serde_json::json!({
                    "id": msg_id,
                    "sessionID": session_id,
                    "role": "user",
                    "time": { "created": ts },
                    "agent": "build",
                    "model": { "providerID": "baton", "modelID": "imported" },
                }),
                Role::Assistant => {
                    let pid = prev_id.clone().unwrap_or_else(|| msg_id.clone());
                    let out_tok = estimate_tokens(&msg.parts);
                    total_output += out_tok;
                    serde_json::json!({
                        "id": msg_id,
                        "sessionID": session_id,
                        "role": "assistant",
                        "time": { "created": ts, "completed": ts + 1 },
                        "parentID": pid,
                        "modelID": "imported",
                        "providerID": "baton",
                        "mode": "build",
                        "agent": "build",
                        "path": { "cwd": cwd, "root": cwd },
                        "cost": 0,
                        "tokens": {
                            "input": context_tokens,
                            "output": out_tok,
                            "reasoning": 0,
                            "cache": { "read": 0, "write": 0 }
                        },
                    })
                }
            };
            context_tokens += estimate_tokens(&msg.parts);

            let mut parts_json: Vec<serde_json::Value> = Vec::new();
            let mut first_text = true;
            for p in &msg.parts {
                let part_id = ids.next("prt", msg.time_created.max(0));
                match p {
                    Part::Text { text } => {
                        let text = if msg.role == Role::System && first_text {
                            format!("[system] {text}")
                        } else {
                            text.clone()
                        };
                        first_text = false;
                        parts_json.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                        }));
                    }
                    Part::Reasoning { text, signature } => {
                        let mut part = serde_json::json!({
                            "type": "reasoning",
                            "text": text,
                            "time": { "start": ts, "end": ts + 1 },
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                        });
                        // The signature must go in `metadata.anthropic.signature`, NOT as a
                        // sibling of `text`: opencode's SessionV1.ReasoningPart schema has no
                        // top-level `signature`, and its importer decodes with Effect Schema's
                        // default `onExcessProperty: "ignore"`, so a top-level key is silently
                        // dropped. `metadata` is where opencode itself keeps it and where its
                        // replay path reads it from (message-v2.ts: part.metadata?.anthropic
                        // ?.signature). Without it Anthropic rejects the replayed thinking
                        // block with "thinking.signature: Field required".
                        if let Some(sig) = signature {
                            part["metadata"] = serde_json::json!({
                                "anthropic": { "signature": sig }
                            });
                        }
                        parts_json.push(part)
                    }
                    Part::ToolCall { name, id, input } => {
                        let call_id = id
                            .clone()
                            .unwrap_or_else(|| format!("call_{}", &Uuid::new_v4().simple().to_string()[..16]));
                        parts_json.push(serde_json::json!({
                            "type": "tool",
                            "callID": call_id,
                            "tool": name,
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                            "state": {
                                "status": "completed",
                                "input": input.clone().unwrap_or(serde_json::Value::Object(Default::default())),
                                "output": "",
                                "title": name,
                                "metadata": {},
                                "time": { "start": ts, "end": ts + 1 },
                            },
                        }));
                        tool_locs.insert(call_id, (out_messages.len(), parts_json.len() - 1));
                    }
                    Part::ToolResult { name, id, output, is_error } => {
                        // Fold into the matching tool part (possibly in an earlier message).
                        let folded = id.as_ref().and_then(|cid| tool_locs.get(cid).copied());
                        let out_text = output.clone().unwrap_or_default();
                        let errored = is_error.unwrap_or(false);
                        match folded {
                            Some((mi, pi)) => {
                                let target = if mi == out_messages.len() {
                                    parts_json.get_mut(pi)
                                } else {
                                    out_messages
                                        .get_mut(mi)
                                        .and_then(|m: &mut serde_json::Value| m.get_mut("parts"))
                                        .and_then(|ps| ps.get_mut(pi))
                                };
                                if let Some(state) = target.and_then(|t| t.get_mut("state")) {
                                    state["output"] = serde_json::json!(out_text);
                                    if errored {
                                        state["status"] = serde_json::json!("error");
                                        state["error"] = serde_json::json!(out_text);
                                    }
                                }
                            }
                            None => parts_json.push(serde_json::json!({
                                "type": "text",
                                "text": format!("[tool result: {}] {}", name, out_text),
                                "id": part_id,
                                "sessionID": session_id,
                                "messageID": msg_id,
                            })),
                        }
                    }
                    Part::Attachment { .. } => parts_json.push(serde_json::json!({
                        "type": "text",
                        "text": "[attachment]",
                        "id": part_id,
                        "sessionID": session_id,
                        "messageID": msg_id,
                    })),
                }
            }

            // A message whose only content folded into an earlier tool part (e.g. a
            // Claude-style user message carrying just tool_results) would be empty —
            // opencode rejects part-less messages, so skip it.
            if parts_json.is_empty() {
                continue;
            }
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
                "model": { "id": "imported", "providerID": "baton" },
                "version": env!("CARGO_PKG_VERSION"),
                "summary": { "additions": 0, "deletions": 0, "files": 0 },
                "cost": 0,
                "tokens": {
                    "input": context_tokens.saturating_sub(total_output),
                    "output": total_output,
                    "reasoning": 0,
                    "cache": { "read": 0, "write": 0 }
                },
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

/// Rough token estimate for a message's parts (~4 chars per token).
fn estimate_tokens(parts: &[Part]) -> u64 {
    let chars: usize = parts
        .iter()
        .map(|p| match p {
            Part::Text { text } | Part::Reasoning { text, .. } => text.len(),
            Part::ToolCall { input, .. } => {
                input.as_ref().map(|v| v.to_string().len()).unwrap_or(0)
            }
            Part::ToolResult { output, .. } => output.as_ref().map(|s| s.len()).unwrap_or(0),
            Part::Attachment { .. } => 0,
        })
        .sum();
    (chars / 4) as u64
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
    // index arithmetic mirrors the SHA-1 spec; iterator forms would obscure it
    #[allow(clippy::needless_range_loop)]
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
    /// Tool name, present on `type: "tool"` parts.
    #[serde(default)]
    tool: Option<String>,
    #[serde(rename = "callID", default)]
    call_id: Option<String>,
    #[serde(default)]
    state: Option<ExportToolState>,
    /// Provider metadata on `type: "reasoning"` parts; opencode keeps the Anthropic
    /// thinking signature at `metadata.anthropic.signature`.
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    /// Tolerated legacy/top-level spelling of the signature (opencode itself never
    /// emits this, but be liberal in what we accept).
    #[serde(default)]
    signature: Option<String>,
}

impl ExportPart {
    /// Signature for a reasoning part, preferring opencode's canonical
    /// `metadata.anthropic.signature` location.
    fn reasoning_signature(&self) -> Option<String> {
        self.metadata
            .as_ref()
            .and_then(|m| m.get("anthropic"))
            .and_then(|a| a.get("signature"))
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .or_else(|| self.signature.clone())
    }
}

#[derive(Debug, serde::Deserialize)]
struct ExportToolState {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
    #[serde(default)]
    output: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Format as _, Message, Role};

    #[test]
    fn sha1_known_vectors() {
        let mut h = Sha1::new();
        h.update(b"abc");
        assert_eq!(hex_encode(&h.finalize()), "a9993e364706816aba3e25717850c26c9cd0d89d");

        let mut h = Sha1::new();
        h.update(b"");
        assert_eq!(hex_encode(&h.finalize()), "da39a3ee5e6b4b0d3255bfef95601890afd80709");

        // >64 bytes to exercise multi-block path
        let mut h = Sha1::new();
        h.update("a".repeat(1000).as_bytes());
        assert_eq!(hex_encode(&h.finalize()), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
    }

    #[test]
    fn write_read_round_trip_preserves_tools_and_roles() {
        let session = Session {
            source_id: "orig".into(),
            origin: Agent::ClaudeCode,
            title: "test".into(),
            time_created: 1000,
            time_updated: 2000,
            directory: Some("/tmp".into()),
            messages: vec![
                Message {
                    role: Role::System,
                    parts: vec![Part::text("be helpful")],
                    time_created: 1000,
                    origin: None,
                },
                Message {
                    role: Role::User,
                    parts: vec![Part::text("hi")],
                    time_created: 1001,
                    origin: None,
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Reasoning { text: "thinking".into(), signature: None },
                        Part::ToolCall {
                            name: "Bash".into(),
                            id: Some("call_1".into()),
                            input: Some(serde_json::json!({"command": "ls"})),
                        },
                    ],
                    time_created: 1002,
                    origin: None,
                },
                Message {
                    // Claude-style: tool result arrives in a following user message
                    role: Role::User,
                    parts: vec![Part::ToolResult {
                        name: "Bash".into(),
                        id: Some("call_1".into()),
                        output: Some("file.txt".into()),
                        is_error: Some(false),
                    }],
                    time_created: 1003,
                    origin: None,
                },
            ],
        };

        let dir = std::env::temp_dir().join(format!("baton-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.json");
        Opencode::write(&session, &path).unwrap();
        let back = Opencode::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(back.messages[0].role, Role::System);
        assert_eq!(back.messages[0].parts[0].as_text(), Some("be helpful"));
        assert_eq!(back.messages[1].role, Role::User);
        let asst = &back.messages[2];
        assert_eq!(asst.role, Role::Assistant);
        assert!(asst.parts.iter().any(|p| matches!(p, Part::Reasoning { text, .. } if text == "thinking")));
        let call = asst.parts.iter().find_map(|p| match p {
            Part::ToolCall { name, id, input } => Some((name.clone(), id.clone(), input.clone())),
            _ => None,
        });
        let (name, id, input) = call.expect("tool call survives round trip");
        assert_eq!(name, "Bash");
        assert_eq!(id.as_deref(), Some("call_1"));
        assert_eq!(input.unwrap()["command"], "ls");
        let result = asst.parts.iter().find_map(|p| match p {
            Part::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        });
        assert_eq!(result.expect("tool result folded into tool part"), Some("file.txt".into()));
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Hello, World!"), "hello--world");
        assert_eq!(slugify(""), "imported");
    }
}

/// Generator for opencode-compatible, TIME-SORTABLE identifiers.
///
/// opencode (packages/schema/src/identifier.ts) builds ids as a 12-hex-char
/// prefix encoding `timestamp_ms * 0x1000 + counter`, followed by 14 random
/// characters from a 62-char alphabet. The prefix is what makes ids sort
/// chronologically as plain strings, and opencode's run loop depends on that:
/// it decides a turn is complete with `lastUser.id < lastAssistant.id`.
///
/// Emitting random uuids here instead is not merely cosmetic — roughly 2.6% of
/// random hex ids sort ABOVE a freshly generated native id, so on a session with
/// hundreds of messages it is effectively certain that some imported message
/// outranks the live conversation. opencode then never exits its run loop and
/// re-requests with the conversation ending on an assistant message, which
/// Anthropic rejects ("does not support assistant message prefill").
struct IdGen {
    last_timestamp: i64,
    counter: u64,
}

impl IdGen {
    const CHARS: &'static [u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    fn new() -> Self {
        Self { last_timestamp: -1, counter: 0 }
    }

    /// `<prefix>_<12 hex time chars><14 random chars>`; monotonic for equal or
    /// out-of-order timestamps, so ordering always follows emission order.
    fn next(&mut self, prefix: &str, timestamp_ms: i64) -> String {
        // Never let a stale timestamp produce a smaller id than the previous one.
        let ts = timestamp_ms.max(self.last_timestamp);
        if ts != self.last_timestamp {
            self.last_timestamp = ts;
            self.counter = 0;
        }
        self.counter += 1;
        // counter shares the low 12 bits with opencode's scheme; wrap into the
        // next millisecond rather than colliding once it overflows.
        if self.counter >= 0x1000 {
            self.last_timestamp += 1;
            self.counter = 1;
        }
        let current = (self.last_timestamp as u128) * 0x1000 + self.counter as u128;
        let mut out = String::with_capacity(prefix.len() + 1 + 26);
        out.push_str(prefix);
        out.push('_');
        for i in 0..6 {
            out.push_str(&format!("{:02x}", ((current >> (40 - 8 * i)) & 0xff) as u8));
        }
        let bytes = Uuid::new_v4();
        for b in bytes.as_bytes().iter().take(14) {
            out.push(Self::CHARS[(*b as usize) % 62] as char);
        }
        out
    }
}

#[cfg(test)]
mod idgen_tests {
    use super::IdGen;

    #[test]
    fn ids_sort_chronologically_and_monotonically() {
        let mut g = IdGen::new();
        let a = g.next("msg", 1_700_000_000_000);
        let b = g.next("msg", 1_700_000_000_000); // same ms
        let c = g.next("msg", 1_700_000_001_000); // later
        let d = g.next("msg", 1_600_000_000_000); // EARLIER (out of order input)
        assert!(a < b, "same-timestamp ids must increase: {a} !< {b}");
        assert!(b < c, "later timestamp must sort higher: {b} !< {c}");
        assert!(c < d, "a stale timestamp must not go backwards: {c} !< {d}");
        assert_eq!(a.len(), "msg_".len() + 26);
        assert!(a.starts_with("msg_"));
    }

    #[test]
    fn ids_outrank_older_and_are_outranked_by_newer_native_ids() {
        // a native opencode id for "now" must sort above ids built from past timestamps
        let mut g = IdGen::new();
        let past = g.next("msg", 1_600_000_000_000);
        let now = g.next("msg", chrono::Utc::now().timestamp_millis());
        assert!(past < now);
    }
}
