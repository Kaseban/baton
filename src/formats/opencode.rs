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

use crate::canonical::{Agent, Format, Message, ModelRef, Part, Role, Session};

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
                model: msg.info.model_ref(),
                summary: false,
            });
        }

        Ok(Session {
            source_id: info.id.clone(),
            origin: Agent::Opencode,
            title: info.title.clone(),
            time_created: info.time.created.unwrap_or(0),
            time_updated: info.time.updated.unwrap_or(0),
            directory: Some(info.directory.clone()),
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages,
        })
    }

    fn write(session: &Session, out_path: &Path) -> anyhow::Result<()> {
        let directory = session.directory.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
                .unwrap_or_else(|| ".".to_string())
        });
        let repo = GitRepo::discover(&directory);
        let worktree = repo
            .as_ref()
            .map(|r| r.worktree.clone())
            .unwrap_or_else(|| directory.clone());
        let models = message_models(&session.messages);
        let session_model = models
            .iter()
            .rev()
            .find_map(|m| m.clone())
            .unwrap_or_else(placeholder_model);
        let now = chrono::Utc::now().timestamp_millis();
        // Identifiers must be TIME-SORTABLE, not random: opencode's run loop decides
        // whether a turn is finished with a raw lexicographic comparison of message
        // ids (`lastUser.id < lastAssistant.id`, packages/opencode/src/session/
        // prompt.ts), and `MessageV2.latest()` picks the "last" message the same way.
        // Random uuids break that ordering, so an imported message can outrank the
        // live ones, the loop never exits, and opencode re-requests with the
        // conversation ending on an assistant message -> Anthropic 400
        // "does not support assistant message prefill".
        let seed = format!("{}:{}", session.origin, session.source_id);
        let mut ids = IdGen::new(now, &seed);
        let session_id = IdGen::session_id(&seed);
        let slug = slugify(&session.title);

        let mut out_messages = Vec::with_capacity(session.messages.len());
        let mut prev_id: Option<String> = None;
        // callID → (out_messages index, parts index) of the emitted "tool" part, so a
        // ToolResult arriving in a later message folds into that part's state.
        let mut tool_locs: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        let mut attachment_count = 0;
        // Rough usage estimates (~4 chars/token): opencode's UI expects real
        // numbers here, and all-zero usage on every message reads as a session
        // that never produced anything.
        let mut context_tokens: u64 = 0;
        let mut total_output: u64 = 0;
        let mut spawned = session.children.iter();
        for (msg, model) in session.messages.iter().zip(&models) {
            let ts = msg.time_created;
            let model = model.clone().unwrap_or_else(|| session_model.clone());
            // opencode's own form of a context summary: a user message holding a
            // compaction part, answered by an assistant message flagged `summary`.
            // `MessageV2.filterCompacted` then replays only the summary and what
            // follows it, as the source agent did.
            if msg.summary {
                let compaction_id = ids.next("msg", ts.max(0));
                out_messages.push(serde_json::json!({
                    "info": {
                        "id": compaction_id,
                        "sessionID": session_id,
                        "role": "user",
                        "time": { "created": ts },
                        "agent": "build",
                        "model": { "providerID": model.provider, "modelID": model.id },
                    },
                    "parts": [{
                        "type": "compaction",
                        "auto": true,
                        "id": ids.next("prt", ts.max(0)),
                        "sessionID": session_id,
                        "messageID": compaction_id,
                    }],
                }));
                prev_id = Some(compaction_id);
                context_tokens = 0;
            }
            let msg_id = ids.next("msg", msg.time_created.max(0));
            let role = if msg.summary { Role::Assistant } else { msg.role };
            let msg_info: serde_json::Value = match role {
                // opencode only accepts user/assistant; system messages are written as
                // user with a "[system] " text prefix (recovered on read).
                Role::User | Role::System => serde_json::json!({
                    "id": msg_id,
                    "sessionID": session_id,
                    "role": "user",
                    "time": { "created": ts },
                    "agent": "build",
                    "model": { "providerID": model.provider, "modelID": model.id },
                }),
                Role::Assistant => {
                    let pid = prev_id.clone().unwrap_or_else(|| msg_id.clone());
                    let out_tok = estimate_tokens(&msg.parts);
                    total_output += out_tok;
                    // opencode only checks a session's size against the model's
                    // context before prompting when the last reply is finished
                    // (`MessageV2.latest`); without `finish` an import too large for
                    // the model is sent whole and rejected instead of compacted.
                    let finish = match msg.parts.last() {
                        Some(Part::ToolCall { .. }) => "tool-calls",
                        _ => "stop",
                    };
                    let mut info = serde_json::json!({
                        "id": msg_id,
                        "sessionID": session_id,
                        "role": "assistant",
                        "time": { "created": ts, "completed": ts + 1 },
                        "finish": finish,
                        "parentID": pid,
                        "modelID": model.id,
                        "providerID": model.provider,
                        "mode": "build",
                        "agent": "build",
                        "path": { "cwd": directory, "root": worktree },
                        "cost": 0,
                        "tokens": {
                            "input": context_tokens,
                            "output": out_tok,
                            "reasoning": 0,
                            "cache": { "read": 0, "write": 0 }
                        },
                    });
                    if msg.summary {
                        info["summary"] = serde_json::json!(true);
                        info["finish"] = serde_json::json!("stop");
                        info["mode"] = serde_json::json!("compaction");
                        info["agent"] = serde_json::json!("compaction");
                    }
                    info
                }
            };
            context_tokens += estimate_tokens(&msg.parts);

            let mut parts_json: Vec<serde_json::Value> = Vec::new();
            let mut first_text = true;
            // The tool part a ToolResult just folded into: attachments right after the
            // result are that tool's output (a screenshot it took, an image it read).
            let mut result_tool: Option<(usize, usize)> = None;
            for p in &msg.parts {
                let part_id = ids.next("prt", msg.time_created.max(0));
                if !matches!(p, Part::Attachment { .. }) {
                    result_tool = None;
                }
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
                            .unwrap_or_else(|| format!("call_{}", &part_id[part_id.len() - 16..]));
                        let input = input
                            .clone()
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        let tool = match msg.origin.unwrap_or(session.origin) {
                            Agent::ClaudeCode => native_tool(name, input),
                            Agent::Cline => {
                                let mut tool =
                                    cline_tool(name, input, session.directory.as_deref());
                                if tool.name == "task"
                                    && let Some(child) = spawned.next()
                                {
                                    tool.metadata["sessionId"] = serde_json::json!(
                                        IdGen::session_id(&format!("{}:{child}", session.origin))
                                    );
                                }
                                tool
                            }
                            _ => NativeTool {
                                name: name.clone(),
                                title: name.clone(),
                                metadata: serde_json::json!({}),
                                input,
                            },
                        };
                        parts_json.push(serde_json::json!({
                            "type": "tool",
                            "callID": call_id,
                            "tool": tool.name,
                            "id": part_id,
                            "sessionID": session_id,
                            "messageID": msg_id,
                            "state": {
                                "status": "completed",
                                "input": tool.input,
                                "output": "",
                                "title": relative_to(&tool.title, session.directory.as_deref()),
                                "metadata": tool.metadata,
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
                        // opencode only keeps attachments on completed tool states.
                        result_tool = folded.filter(|_| !errored);
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
                                if let Some(part) = target {
                                    let tool = part["tool"].as_str().unwrap_or_default().to_string();
                                    let state = &mut part["state"];
                                    state["output"] = serde_json::json!(out_text);
                                    // opencode's bash view renders the command's output from
                                    // its metadata, not from `state.output`.
                                    if tool == "bash" {
                                        state["metadata"]["output"] = serde_json::json!(out_text);
                                    }
                                    // ...and its question view the answers.
                                    if tool == "question" {
                                        state["metadata"]["answers"] =
                                            serde_json::json!([[question_answer(&out_text)]]);
                                    }
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
                    Part::Attachment { mime, path, data } => {
                        let url = match (data, path) {
                            (Some(data), _) => format!("data:{mime};base64,{data}"),
                            (None, Some(path)) => format!("file://{path}"),
                            (None, None) => {
                                parts_json.push(serde_json::json!({
                                    "type": "text",
                                    "text": "[attachment]",
                                    "id": part_id,
                                    "sessionID": session_id,
                                    "messageID": msg_id,
                                }));
                                continue;
                            }
                        };
                        attachment_count += 1;
                        let filename = path
                            .as_deref()
                            .and_then(|p| Path::new(p).file_name())
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| {
                                let (kind, ext) = mime.split_once('/').unwrap_or(("file", "bin"));
                                format!("{kind}-{attachment_count:03}.{ext}")
                            });
                        let file_part = |message_id: &serde_json::Value| {
                            serde_json::json!({
                                "type": "file",
                                "mime": mime,
                                "filename": filename,
                                "url": url,
                                "id": part_id,
                                "sessionID": session_id,
                                "messageID": message_id,
                            })
                        };
                        let tool_part = result_tool.and_then(|(mi, pi)| {
                            if mi == out_messages.len() {
                                parts_json.get_mut(pi)
                            } else {
                                out_messages
                                    .get_mut(mi)
                                    .and_then(|m: &mut serde_json::Value| m.get_mut("parts"))
                                    .and_then(|ps| ps.get_mut(pi))
                            }
                        });
                        match tool_part {
                            Some(tool) => {
                                let file = file_part(&tool["messageID"]);
                                let state = &mut tool["state"];
                                if !state["attachments"].is_array() {
                                    state["attachments"] = serde_json::json!([]);
                                }
                                if let Some(list) = state["attachments"].as_array_mut() {
                                    list.push(file);
                                }
                            }
                            None => parts_json.push(file_part(&serde_json::json!(msg_id))),
                        }
                    }
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

        let mut export = serde_json::json!({
            "info": {
                "id": session_id,
                "slug": slug,
                "projectID": repo.as_ref().and_then(|r| r.root_commit.clone()).unwrap_or_else(|| "global".to_string()),
                "directory": directory,
                "path": repo.as_ref().map(|r| r.relative_path.clone()).unwrap_or_default(),
                "title": format!(
                    "{}{}",
                    session
                        .title_prefix
                        .clone()
                        .unwrap_or_else(|| format!("[{}] ", session.origin)),
                    session.title
                ),
                "agent": "build",
                "model": { "id": session_model.id, "providerID": session_model.provider },
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
        if let Some(parent) = &session.parent {
            export["info"]["parentID"] =
                serde_json::json!(IdGen::session_id(&format!("{}:{parent}", session.origin)));
        }

        let pretty = serde_json::to_string_pretty(&export)?;
        std::fs::write(out_path, pretty)
            .with_context(|| format!("writing {}", out_path.display()))?;
        Ok(())
    }
}

/// A tool call as opencode's own tool would have recorded it.
struct NativeTool {
    name: String,
    input: serde_json::Value,
    title: String,
    metadata: serde_json::Value,
}

/// Map a Claude Code tool call onto the opencode tool that does the same job, so
/// opencode renders it with that tool's view instead of as an unknown tool. The
/// input keys are renamed to opencode's (`file_path` -> `filePath`, ...); keys
/// with no opencode counterpart are kept as they are. Tools opencode has no
/// equivalent for keep their Claude Code name.
fn native_tool(name: &str, input: serde_json::Value) -> NativeTool {
    let (native, renames): (&str, &[(&str, &str)]) = match name {
        "Bash" => ("bash", &[]),
        "Read" => ("read", &[("file_path", "filePath")]),
        "Write" => ("write", &[("file_path", "filePath")]),
        "Edit" => (
            "edit",
            &[
                ("file_path", "filePath"),
                ("old_string", "oldString"),
                ("new_string", "newString"),
                ("replace_all", "replaceAll"),
            ],
        ),
        "Glob" => ("glob", &[]),
        "Grep" => ("grep", &[("glob", "include")]),
        "WebFetch" => ("webfetch", &[]),
        "WebSearch" => ("websearch", &[]),
        "TodoWrite" => ("todowrite", &[]),
        _ => {
            return NativeTool {
                name: name.to_string(),
                title: name.to_string(),
                metadata: serde_json::json!({}),
                input,
            };
        }
    };
    let input = match input {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let key = renames
                        .iter()
                        .find(|(from, _)| *from == key)
                        .map_or(key, |(_, to)| to.to_string());
                    (key, value)
                })
                .collect(),
        ),
        other => other,
    };
    let field = |key: &str| input.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let title = match native {
        "bash" => field("description").or_else(|| field("command")),
        "read" | "write" | "edit" => field("filePath"),
        "glob" | "grep" => field("pattern"),
        "webfetch" => field("url"),
        "websearch" => field("query"),
        "todowrite" => input.get("todos").and_then(|t| t.as_array()).map(|todos| {
            let open = todos.iter().filter(|t| t["status"] != "completed").count();
            format!("{open} todos")
        }),
        _ => None,
    }
    .unwrap_or_else(|| native.to_string());
    // opencode's todo view lists the todos from the tool's metadata.
    let metadata = match native {
        "todowrite" => {
            serde_json::json!({ "todos": input.get("todos").cloned().unwrap_or_default() })
        }
        _ => serde_json::json!({}),
    };
    NativeTool {
        name: native.to_string(),
        input,
        title,
        metadata,
    }
}

/// Map a Cline-family (Cline, Roo Code, Kilo Code, Zoo Code) tool call onto
/// the opencode tool that does the same job, so opencode renders it with that
/// tool's view. Relative paths are resolved against the session directory, as
/// the agent resolved them against its workspace. Tools opencode has no
/// equivalent for, and calls whose input does not fit one, keep their name and
/// input.
fn cline_tool(name: &str, input: serde_json::Value, directory: Option<&str>) -> NativeTool {
    let field = |key: &str| input.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let path = |p: &str| resolve_path(p, directory);
    let native = |native: &str, title: Option<String>, input, metadata| NativeTool {
        name: native.to_string(),
        title: title.unwrap_or_else(|| native.to_string()),
        input,
        metadata,
    };
    let edit = |file: String, blocks: Vec<(usize, String, String)>, original_diff: Option<String>| {
        let diff = unified_diff(&file, &blocks);
        let input = match (blocks.as_slice(), original_diff) {
            ([(_, old, new)], _) => {
                serde_json::json!({ "filePath": file, "oldString": old, "newString": new })
            }
            (_, diff) => serde_json::json!({ "filePath": file, "diff": diff }),
        };
        native("edit", Some(file), input, serde_json::json!({ "diff": diff }))
    };
    let tool = match name {
        "execute_command" => field("command").map(|command| {
            let mut input = serde_json::json!({ "command": command });
            if let Some(cwd) = field("cwd").filter(|c| !c.is_empty()) {
                input["workdir"] = serde_json::json!(path(&cwd));
            }
            native("bash", Some(command), input, serde_json::json!({}))
        }),
        "read_file" => match cline_file_paths(&input).as_slice() {
            [file] => {
                let file = path(file);
                Some(native("read", Some(file.clone()), serde_json::json!({ "filePath": file }), serde_json::json!({})))
            }
            _ => None,
        },
        "write_to_file" => field("path").map(|file| {
            let file = path(&file);
            let input = serde_json::json!({ "filePath": file, "content": field("content").unwrap_or_default() });
            native("write", Some(file), input, serde_json::json!({}))
        }),
        "apply_diff" | "replace_in_file" => match (cline_file_paths(&input).as_slice(), cline_diff(&input)) {
            ([file], Some(diff)) => {
                let blocks = search_replace_blocks(&diff);
                (!blocks.is_empty()).then(|| edit(path(file), blocks, Some(diff)))
            }
            _ => None,
        },
        "search_and_replace" | "edit" | "edit_file" => {
            let file = field("path").or_else(|| field("file_path"));
            let old = field("search").or_else(|| field("old_string"));
            let new = field("replace").or_else(|| field("new_string"));
            match (file, old, new) {
                (Some(file), Some(old), Some(new)) => Some(edit(path(&file), vec![(1, old, new)], None)),
                _ => None,
            }
        }
        "search_files" => field("regex").map(|pattern| {
            let mut input = serde_json::json!({ "pattern": pattern });
            if let Some(dir) = field("path") {
                input["path"] = serde_json::json!(path(&dir));
            }
            if let Some(include) = field("file_pattern") {
                input["include"] = serde_json::json!(include);
            }
            native("grep", Some(pattern), input, serde_json::json!({}))
        }),
        "update_todo_list" => field("todos").map(|list| {
            let todos = cline_todos(&list);
            let open = todos.iter().filter(|t| t["status"] != "completed").count();
            native(
                "todowrite",
                Some(format!("{open} todos")),
                serde_json::json!({ "todos": todos }),
                serde_json::json!({ "todos": todos }),
            )
        }),
        "ask_followup_question" => field("question").map(|question| {
            let options: Vec<serde_json::Value> = match input.get("follow_up") {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|i| i.get("text").and_then(|t| t.as_str()).or(i.as_str()))
                    .map(|label| serde_json::json!({ "label": label, "description": "" }))
                    .collect(),
                Some(serde_json::Value::String(xml)) => xml_values(xml, "suggest")
                    .into_iter()
                    .map(|label| serde_json::json!({ "label": label, "description": "" }))
                    .collect(),
                _ => Vec::new(),
            };
            let input = serde_json::json!({ "questions": [{
                "question": question,
                "header": "Question",
                "options": options,
            }]});
            native("question", None, input, serde_json::json!({}))
        }),
        "new_task" => field("message").map(|prompt| {
            let description: String = prompt
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("subtask")
                .chars()
                .take(60)
                .collect();
            let input = serde_json::json!({
                "description": description,
                "prompt": prompt,
                "subagent_type": field("mode").unwrap_or_else(|| "general".to_string()),
            });
            native("task", Some(description), input, serde_json::json!({}))
        }),
        "use_mcp_tool" => match (field("server_name"), field("tool_name")) {
            (Some(server), Some(tool)) => {
                let arguments = match input.get("arguments") {
                    Some(serde_json::Value::String(raw)) => serde_json::from_str(raw)
                        .ok()
                        .filter(serde_json::Value::is_object)
                        .unwrap_or_else(|| serde_json::json!({ "arguments": raw })),
                    Some(args @ serde_json::Value::Object(_)) => args.clone(),
                    _ => serde_json::json!({}),
                };
                let name = mcp_tool_name(&server, &tool);
                Some(native(&name, None, arguments, serde_json::json!({})))
            }
            _ => None,
        },
        _ => name.strip_prefix("mcp--").and_then(|rest| {
            let (server, tool) = rest.split_once("--")?;
            let decode = |s: &str| s.replace("___", "-");
            let name = mcp_tool_name(&decode(server), &decode(tool));
            Some(native(&name, None, input.clone(), serde_json::json!({})))
        }),
    };
    tool.unwrap_or_else(|| NativeTool {
        name: name.to_string(),
        title: name.to_string(),
        metadata: serde_json::json!({}),
        input,
    })
}

/// opencode's name for tool `tool` of MCP server `server`
/// (packages/opencode/src/mcp/catalog.ts `toolName`).
fn mcp_tool_name(server: &str, tool: &str) -> String {
    let sanitize = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect::<String>()
    };
    format!("{}_{}", sanitize(server), sanitize(tool))
}

/// Every file a Cline-family file tool names: `path`, native `files[].path`, or
/// the XML protocol's `args.file[].path`.
fn cline_file_paths(input: &serde_json::Value) -> Vec<String> {
    let path_of = |v: &serde_json::Value| {
        v.as_str()
            .or_else(|| v.get("path").and_then(|p| p.as_str()))
            .map(str::to_string)
    };
    let many = |v: Option<&serde_json::Value>| match v {
        Some(serde_json::Value::Array(items)) => items.iter().filter_map(path_of).collect(),
        Some(one) => path_of(one).into_iter().collect(),
        None => Vec::new(),
    };
    if let Some(p) = input.get("path").and_then(|p| p.as_str()) {
        return vec![p.to_string()];
    }
    let files = many(input.get("files"));
    if !files.is_empty() {
        return files;
    }
    many(input.get("args").and_then(|a| a.get("file")))
}

/// The diff of a single-file `apply_diff` / `replace_in_file`: `diff`, or the
/// XML protocol's `args.file.diff`, which may hold one or more
/// `{ content, start_line }` with the content in a code fence or CDATA.
fn cline_diff(input: &serde_json::Value) -> Option<String> {
    if let Some(diff) = input.get("diff").and_then(|d| d.as_str()) {
        return Some(diff.to_string());
    }
    let unwrap = |content: &str| {
        let content = content.trim();
        let content = content
            .strip_prefix("<![CDATA[")
            .and_then(|c| c.strip_suffix("]]>"))
            .unwrap_or(content);
        let content = content.trim();
        match content.strip_prefix("```") {
            Some(fenced) => {
                let body = fenced.split_once('\n').map_or("", |(_, b)| b);
                body.trim_end().strip_suffix("```").unwrap_or(body).to_string()
            }
            None => content.to_string(),
        }
    };
    let one = |d: &serde_json::Value| match d {
        serde_json::Value::String(s) => Some(unwrap(s)),
        serde_json::Value::Object(_) => {
            let content = unwrap(d.get("content")?.as_str()?);
            let start = d.get("start_line").and_then(|s| s.as_str()).map(str::trim);
            Some(match start {
                Some(n) if !content.contains(":start_line:") => {
                    content.replacen(" SEARCH\n", &format!(" SEARCH\n:start_line:{n}\n"), 1)
                }
                _ => content,
            })
        }
        _ => None,
    };
    let diffs = match input.get("args")?.get("file")?.get("diff")? {
        serde_json::Value::Array(items) => items.iter().filter_map(one).collect(),
        single => vec![one(single)?],
    };
    (!diffs.is_empty()).then(|| diffs.join("\n"))
}

/// The SEARCH/REPLACE blocks of a Roo `apply_diff` or Cline `replace_in_file`
/// diff, as `(start line, search, replace)`.
fn search_replace_blocks(diff: &str) -> Vec<(usize, String, String)> {
    enum State {
        Outside,
        Search,
        Replace,
    }
    let mut blocks = Vec::new();
    let (mut state, mut start, mut old, mut new) = (State::Outside, 1, Vec::new(), Vec::new());
    for line in diff.lines() {
        let marker = line.trim_end();
        match state {
            State::Outside => {
                if marker.ends_with(" SEARCH")
                    && (marker.starts_with("<<<<<<<") || marker.starts_with("-------"))
                {
                    (state, start, old, new) = (State::Search, 1, Vec::new(), Vec::new());
                }
            }
            State::Search => {
                if let Some(n) = marker.strip_prefix(":start_line:") {
                    start = n.trim().parse().unwrap_or(1);
                } else if marker.starts_with(":end_line:") || (marker == "-------" && old.is_empty()) {
                } else if marker == "=======" {
                    state = State::Replace;
                } else {
                    old.push(line);
                }
            }
            State::Replace => {
                if marker.ends_with(" REPLACE")
                    && (marker.starts_with(">>>>>>>") || marker.starts_with("+++++++"))
                {
                    blocks.push((start, old.join("\n"), new.join("\n")));
                    state = State::Outside;
                } else {
                    new.push(line);
                }
            }
        }
    }
    blocks
}

/// A unified diff of SEARCH/REPLACE blocks, shaped like the patch opencode's
/// edit tool records in its metadata (jsdiff `createTwoFilesPatch`).
fn unified_diff(file: &str, blocks: &[(usize, String, String)]) -> String {
    let mut out = format!(
        "Index: {file}\n===================================================================\n--- {file}\n+++ {file}\n"
    );
    for (start, old, new) in blocks {
        let old: Vec<&str> = if old.is_empty() { Vec::new() } else { old.split('\n').collect() };
        let new: Vec<&str> = if new.is_empty() { Vec::new() } else { new.split('\n').collect() };
        out.push_str(&format!("@@ -{start},{} +{start},{} @@\n", old.len(), new.len()));
        for line in old {
            out.push_str(&format!("-{line}\n"));
        }
        for line in new {
            out.push_str(&format!("+{line}\n"));
        }
    }
    out
}

/// Roo's markdown checklist (`[x] done`, `[-] doing`, `[ ] todo`) as opencode todos.
fn cline_todos(list: &str) -> Vec<serde_json::Value> {
    list.lines()
        .filter_map(|line| {
            let line = line.trim().trim_start_matches("- ").trim_start();
            let (status, content) = if let Some(rest) = line.strip_prefix("[x]").or_else(|| line.strip_prefix("[X]")) {
                ("completed", rest)
            } else if let Some(rest) = line.strip_prefix("[-]") {
                ("in_progress", rest)
            } else if let Some(rest) = line.strip_prefix("[ ]") {
                ("pending", rest)
            } else {
                return None;
            };
            Some(serde_json::json!({ "content": content.trim(), "status": status }))
        })
        .collect()
}

/// Inner text of every `<tag>...</tag>` in `text`.
fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(after[..end].trim().to_string());
        rest = &after[end + close.len()..];
    }
    out
}

/// The user's answer in an `ask_followup_question` result (`<answer>...</answer>`).
fn question_answer(output: &str) -> String {
    xml_values(output, "answer")
        .into_iter()
        .next()
        .unwrap_or_else(|| output.trim().to_string())
}

/// `path` made absolute against `directory` (lexically, `..` folded away) when
/// it is relative and the directory is known.
fn resolve_path(path: &str, directory: Option<&str>) -> String {
    let absolute = path.starts_with(['/', '\\', '~'])
        || path.as_bytes().get(1) == Some(&b':');
    let Some(dir) = directory.filter(|_| !absolute) else {
        return path.to_string();
    };
    let mut parts: Vec<&str> = dir.split('/').collect();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    if joined.is_empty() { "/".to_string() } else { joined }
}

/// `path` relative to the session directory when it lies inside it, as opencode
/// titles its own file tools; anything else unchanged.
fn relative_to(path: &str, directory: Option<&str>) -> String {
    directory
        .and_then(|dir| Path::new(path).strip_prefix(dir).ok())
        .map(|rel| rel.to_string_lossy().into_owned())
        .filter(|rel| !rel.is_empty())
        .unwrap_or_else(|| path.to_string())
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

/// Placeholder for a session whose source recorded no model at all. opencode
/// requires one on every message; resuming then prompts on the default model.
fn placeholder_model() -> ModelRef {
    ModelRef {
        provider: "baton".to_string(),
        id: "imported".to_string(),
    }
}

/// The model to record on each message. opencode stores a model on user messages
/// too, and a resumed session continues on the model of its last user message
/// (packages/tui/src/component/prompt/index.tsx), so a user message takes the
/// model of the reply that answered it, or failing that the last one before it.
fn message_models(messages: &[Message]) -> Vec<Option<ModelRef>> {
    let mut models: Vec<Option<ModelRef>> = messages.iter().map(|m| m.model.clone()).collect();
    let mut answered_by: Option<ModelRef> = None;
    for (model, msg) in models.iter_mut().zip(messages).rev() {
        if msg.role == Role::Assistant {
            answered_by = model.clone().or(answered_by);
        } else if model.is_none() {
            model.clone_from(&answered_by);
        }
    }
    let mut last_seen: Option<ModelRef> = None;
    for model in &mut models {
        match model {
            Some(m) => last_seen = Some(m.clone()),
            None => model.clone_from(&last_seen),
        }
    }
    models
}

/// The git repository containing a session's directory, when it exists on this
/// machine.
///
/// opencode resolves the project itself on `opencode import`, binding the session
/// to the project of the directory the import runs in, so the `projectID` written
/// here only matters to opencode releases older than that. For those it is the
/// repository's first root commit, the scheme they identify projects by, and
/// "global" outside a repository.
struct GitRepo {
    worktree: String,
    /// Session directory relative to `worktree`, as opencode's `info.path`.
    relative_path: String,
    root_commit: Option<String>,
}

impl GitRepo {
    fn discover(directory: &str) -> Option<Self> {
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(args)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let worktree = git(&["rev-parse", "--show-toplevel"])?.trim().to_string();
        let relative_path = std::fs::canonicalize(directory)
            .ok()
            .and_then(|dir| {
                dir.strip_prefix(&worktree)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
            })
            .unwrap_or_default();
        let mut roots: Vec<String> = git(&["rev-list", "--max-parents=0", "HEAD"])
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        roots.sort();
        Some(Self {
            worktree,
            relative_path,
            root_commit: roots.into_iter().next(),
        })
    }
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
    /// Assistant messages carry the model flat...
    #[serde(rename = "providerID", default)]
    provider_id: Option<String>,
    #[serde(rename = "modelID", default)]
    model_id: Option<String>,
    /// ...user messages nest it.
    #[serde(default)]
    model: Option<UserModel>,
}

#[derive(Debug, serde::Deserialize)]
struct UserModel {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
}

impl MessageInfo {
    fn model_ref(&self) -> Option<ModelRef> {
        let (provider, id) = match (&self.provider_id, &self.model_id, &self.model) {
            (Some(provider), Some(id), _) => (provider.clone(), id.clone()),
            (_, _, Some(m)) => (m.provider_id.clone(), m.model_id.clone()),
            _ => return None,
        };
        Some(ModelRef { provider, id }).filter(|m| *m != placeholder_model())
    }
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
    fn write_read_round_trip_preserves_tools_and_roles() {
        let session = Session {
            source_id: "orig".into(),
            origin: Agent::ClaudeCode,
            title: "test".into(),
            time_created: 1000,
            time_updated: 2000,
            directory: Some("/tmp".into()),
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages: vec![
                Message {
                    role: Role::System,
                    parts: vec![Part::text("be helpful")],
                    time_created: 1000,
                    origin: None,
                    model: None,
                    summary: false,
                },
                Message {
                    role: Role::User,
                    parts: vec![Part::text("hi")],
                    time_created: 1001,
                    origin: None,
                    model: None,
                    summary: false,
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
                    model: None,
                    summary: false,
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
                    model: None,
                    summary: false,
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
        assert_eq!(
            name, "bash",
            "a Claude Code tool comes back as its opencode equivalent"
        );
        assert_eq!(id.as_deref(), Some("call_1"));
        assert_eq!(input.unwrap()["command"], "ls");
        let result = asst.parts.iter().find_map(|p| match p {
            Part::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        });
        assert_eq!(result.expect("tool result folded into tool part"), Some("file.txt".into()));
    }

    fn opus() -> ModelRef {
        ModelRef {
            provider: "anthropic".into(),
            id: "claude-opus-4-8".into(),
        }
    }

    fn message(role: Role, text: &str, model: Option<ModelRef>) -> Message {
        Message {
            role,
            parts: vec![Part::text(text)],
            time_created: 1000,
            origin: None,
            model,
            summary: false,
        }
    }

    fn write_export(session: &Session, name: &str) -> serde_json::Value {
        let dir = std::env::temp_dir().join(format!("baton-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.json");
        Opencode::write(session, &path).unwrap();
        let export = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        export
    }

    fn read_back(export: &serde_json::Value, name: &str) -> Session {
        let dir =
            std::env::temp_dir().join(format!("baton-test-back-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.json");
        std::fs::write(&path, export.to_string()).unwrap();
        let session = Opencode::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        session
    }

    fn session_in(directory: &str, messages: Vec<Message>) -> Session {
        Session {
            source_id: "orig".into(),
            origin: Agent::ClaudeCode,
            title: "Real title".into(),
            time_created: 1000,
            time_updated: 2000,
            directory: Some(directory.into()),
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages,
        }
    }

    #[test]
    fn write_carries_the_source_model_onto_every_message() {
        let sonnet = ModelRef {
            provider: "anthropic".into(),
            id: "claude-sonnet-5".into(),
        };
        let session = session_in(
            "/nonexistent/baton-dir",
            vec![
                message(Role::User, "q1", None),
                message(Role::Assistant, "a1", Some(opus())),
                message(Role::User, "q2", None),
                message(Role::Assistant, "a2", Some(sonnet.clone())),
                message(Role::User, "q3, never answered", None),
            ],
        );
        let export = write_export(&session, "models");
        let msgs = export["messages"].as_array().unwrap();
        let user_model = |i: usize| {
            msgs[i]["info"]["model"]["modelID"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            user_model(0),
            "claude-opus-4-8",
            "a prompt takes the model that answered it"
        );
        assert_eq!(msgs[1]["info"]["providerID"], "anthropic");
        assert_eq!(msgs[1]["info"]["modelID"], "claude-opus-4-8");
        assert_eq!(user_model(2), "claude-sonnet-5");
        assert_eq!(
            user_model(4),
            "claude-sonnet-5",
            "an unanswered prompt keeps the last model"
        );
        assert_eq!(msgs[4]["info"]["model"]["providerID"], "anthropic");
        assert_eq!(
            export["info"]["model"],
            serde_json::json!({ "id": "claude-sonnet-5", "providerID": "anthropic" })
        );

        let back = read_back(&export, "models");
        assert_eq!(back.messages[1].model, Some(opus()));
        assert_eq!(back.messages[3].model, Some(sonnet));
    }

    #[test]
    fn write_without_any_model_keeps_the_placeholder() {
        let export = write_export(
            &session_in(
                "/nonexistent/baton-dir",
                vec![message(Role::User, "q", None)],
            ),
            "no-model",
        );
        assert_eq!(
            export["messages"][0]["info"]["model"]["providerID"],
            "baton"
        );
        let back = read_back(&export, "no-model");
        assert_eq!(
            back.messages[0].model, None,
            "the placeholder is not read back as a model"
        );
    }

    #[test]
    fn write_binds_to_the_session_directory_outside_git() {
        let session = session_in(
            "/nonexistent/baton-dir",
            vec![
                message(Role::User, "q", None),
                message(Role::Assistant, "a", Some(opus())),
            ],
        );
        let export = write_export(&session, "nogit");
        assert_eq!(export["info"]["directory"], "/nonexistent/baton-dir");
        assert_eq!(export["info"]["projectID"], "global");
        assert_eq!(export["info"]["title"], "[claude-code] Real title");
        let path = &export["messages"][1]["info"]["path"];
        assert_eq!(path["cwd"], "/nonexistent/baton-dir");
        assert_eq!(path["root"], "/nonexistent/baton-dir");
    }

    #[test]
    fn write_binds_to_the_git_repo_of_the_session_directory() {
        let repo = std::env::temp_dir().join(format!("baton-test-repo-{}", std::process::id()));
        let sub = repo.join("doc").join("ui");
        std::fs::create_dir_all(&sub).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ]);
        let root_commit = git(&["rev-parse", "HEAD"]);
        let toplevel = git(&["rev-parse", "--show-toplevel"]);

        let session = session_in(
            sub.to_str().unwrap(),
            vec![
                message(Role::User, "q", None),
                message(Role::Assistant, "a", Some(opus())),
            ],
        );
        let export = write_export(&session, "git");
        std::fs::remove_dir_all(&repo).ok();

        assert_eq!(export["info"]["directory"], sub.to_str().unwrap());
        assert_eq!(export["info"]["projectID"], root_commit.as_str());
        assert_eq!(export["info"]["path"], "doc/ui");
        let path = &export["messages"][1]["info"]["path"];
        assert_eq!(path["cwd"], sub.to_str().unwrap());
        assert_eq!(path["root"], toplevel.as_str());
    }

    #[test]
    fn write_turns_images_into_file_parts() {
        let png = Part::Attachment {
            mime: "image/png".into(),
            path: None,
            data: Some("UE5H".into()),
        };
        let jpeg = Part::Attachment {
            mime: "image/jpeg".into(),
            path: None,
            data: Some("SlBH".into()),
        };
        let message = |role, parts, time_created| Message {
            role,
            parts,
            time_created,
            origin: None,
            model: None,
            summary: false,
        };
        let call = |id: &str| Part::ToolCall {
            name: "Read".into(),
            id: Some(id.into()),
            input: None,
        };
        let result = |id: &str, is_error| Part::ToolResult {
            name: "tool".into(),
            id: Some(id.into()),
            output: Some("out".into()),
            is_error: Some(is_error),
        };
        let session = Session {
            source_id: "orig".into(),
            origin: Agent::ClaudeCode,
            title: "images".into(),
            time_created: 1000,
            time_updated: 1004,
            directory: Some("/tmp".into()),
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages: vec![
                message(
                    Role::User,
                    vec![png.clone(), Part::text("what is this?")],
                    1000,
                ),
                message(
                    Role::Assistant,
                    vec![call("call_ok"), call("call_err")],
                    1001,
                ),
                message(
                    Role::User,
                    vec![result("call_ok", false), jpeg.clone()],
                    1002,
                ),
                message(Role::User, vec![result("call_err", true), jpeg], 1003),
            ],
        };

        let dir = std::env::temp_dir().join(format!("baton-test-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("images.json");
        Opencode::write(&session, &path).unwrap();
        let export: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let msgs = export["messages"].as_array().unwrap();
        let pasted = &msgs[0]["parts"][0];
        assert_eq!(pasted["type"], "file");
        assert_eq!(pasted["mime"], "image/png");
        assert_eq!(pasted["url"], "data:image/png;base64,UE5H");
        assert_eq!(pasted["filename"], "image-001.png");
        assert_eq!(pasted["messageID"], msgs[0]["info"]["id"]);

        let assistant = &msgs[1];
        let attachments = assistant["parts"][0]["state"]["attachments"]
            .as_array()
            .unwrap();
        assert_eq!(
            attachments.len(),
            1,
            "a tool result's image stays with that tool"
        );
        assert_eq!(attachments[0]["url"], "data:image/jpeg;base64,SlBH");
        assert_eq!(attachments[0]["messageID"], assistant["info"]["id"]);

        assert_eq!(assistant["parts"][1]["state"]["status"], "error");
        assert!(assistant["parts"][1]["state"].get("attachments").is_none());
        assert_eq!(msgs.len(), 3, "the successful result folded away entirely");
        let kept = &msgs[2]["parts"][0];
        assert_eq!(
            kept["type"], "file",
            "an errored tool cannot hold it, so the image stays in its message"
        );
        assert_eq!(kept["url"], "data:image/jpeg;base64,SlBH");
    }

    #[test]
    fn write_maps_claude_code_tools_to_opencode_tools() {
        let call = |name: &str, id: &str, input: serde_json::Value| Part::ToolCall {
            name: name.into(),
            id: Some(id.into()),
            input: Some(input),
        };
        let result = |id: &str, output: &str| Part::ToolResult {
            name: "tool".into(),
            id: Some(id.into()),
            output: Some(output.into()),
            is_error: Some(false),
        };
        let message = |role, parts| Message {
            role,
            parts,
            time_created: 1000,
            origin: Some(Agent::ClaudeCode),
            model: None,
            summary: false,
        };
        let edit = serde_json::json!({
            "file_path": "/repo/src/a.rs",
            "old_string": "a",
            "new_string": "b",
            "replace_all": true,
        });
        let todos = serde_json::json!({ "todos": [
            { "content": "one", "status": "completed", "activeForm": "Doing one" },
            { "content": "two", "status": "pending", "activeForm": "Doing two" },
        ]});
        let session = Session {
            source_id: "orig".into(),
            origin: Agent::ClaudeCode,
            title: "tools".into(),
            time_created: 1000,
            time_updated: 1000,
            directory: Some("/repo".into()),
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages: vec![
                message(
                    Role::Assistant,
                    vec![
                        call(
                            "Bash",
                            "c1",
                            serde_json::json!({ "command": "ls", "description": "List files" }),
                        ),
                        call("Edit", "c2", edit),
                        call(
                            "Grep",
                            "c3",
                            serde_json::json!({ "pattern": "fn", "glob": "*.rs", "-n": true }),
                        ),
                        call("TodoWrite", "c4", todos.clone()),
                        call("SendUserFile", "c5", serde_json::json!({ "files": [] })),
                    ],
                ),
                message(
                    Role::User,
                    vec![result("c1", "a.rs"), result("c2", "edited")],
                ),
            ],
        };

        let dir = std::env::temp_dir().join(format!("baton-test-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tools.json");
        Opencode::write(&session, &path).unwrap();
        let export: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let parts = &export["messages"][0]["parts"];
        assert_eq!(parts[0]["tool"], "bash");
        assert_eq!(parts[0]["state"]["title"], "List files");
        assert_eq!(
            parts[0]["state"]["metadata"]["output"], "a.rs",
            "bash output is rendered from metadata"
        );

        assert_eq!(parts[1]["tool"], "edit");
        assert_eq!(
            parts[1]["state"]["input"],
            serde_json::json!({ "filePath": "/repo/src/a.rs", "oldString": "a", "newString": "b", "replaceAll": true })
        );
        assert_eq!(parts[1]["state"]["title"], "src/a.rs");
        assert_eq!(parts[1]["state"]["output"], "edited");
        assert!(parts[1]["state"]["metadata"].get("output").is_none());

        assert_eq!(parts[2]["tool"], "grep");
        assert_eq!(
            parts[2]["state"]["input"],
            serde_json::json!({ "pattern": "fn", "include": "*.rs", "-n": true })
        );

        assert_eq!(parts[3]["tool"], "todowrite");
        assert_eq!(parts[3]["state"]["title"], "1 todos");
        assert_eq!(parts[3]["state"]["metadata"]["todos"], todos["todos"]);

        assert_eq!(
            parts[4]["tool"], "SendUserFile",
            "no opencode equivalent: left as is"
        );
        assert_eq!(
            parts[4]["state"]["input"],
            serde_json::json!({ "files": [] })
        );
    }

    #[test]
    fn write_keeps_tool_names_from_other_agents() {
        let session = Session {
            source_id: "orig".into(),
            origin: Agent::Codex,
            title: "codex".into(),
            time_created: 1000,
            time_updated: 1000,
            directory: None,
            title_prefix: None,
            parent: None,
            children: Vec::new(),
            messages: vec![Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall {
                    name: "Read".into(),
                    id: Some("c1".into()),
                    input: Some(serde_json::json!({ "file_path": "/x" })),
                }],
                time_created: 1000,
                origin: None,
                model: None,
                summary: false,
            }],
        };
        let dir =
            std::env::temp_dir().join(format!("baton-test-codex-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tools.json");
        Opencode::write(&session, &path).unwrap();
        let export: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let part = &export["messages"][0]["parts"][0];
        assert_eq!(part["tool"], "Read");
        assert_eq!(
            part["state"]["input"],
            serde_json::json!({ "file_path": "/x" })
        );
    }

    #[test]
    fn write_is_deterministic_and_honours_the_title_prefix() {
        let mut session = session_in(
            "/nonexistent/baton-dir",
            vec![
                message(Role::User, "q", None),
                message(Role::Assistant, "a", Some(opus())),
            ],
        );
        let first = write_export(&session, "det-1");
        let again = write_export(&session, "det-2");
        assert_eq!(first["info"]["id"], again["info"]["id"]);
        assert_eq!(first["messages"], again["messages"], "same ids on every conversion");
        assert!(first["info"]["id"].as_str().unwrap().starts_with("ses_"));
        assert_eq!(first["info"]["id"].as_str().unwrap().len(), "ses_".len() + 26);

        session.source_id = "other".into();
        session.title_prefix = Some("[import:roo] ".into());
        let other = write_export(&session, "det-3");
        assert_ne!(other["info"]["id"], first["info"]["id"]);
        assert_ne!(other["messages"][0]["info"]["id"], first["messages"][0]["info"]["id"]);
        assert_eq!(other["info"]["title"], "[import:roo] Real title");
    }

    #[test]
    fn write_maps_cline_tools_summaries_and_subtasks() {
        let call = |name: &str, id: &str, input: serde_json::Value| Part::ToolCall {
            name: name.into(),
            id: Some(id.into()),
            input: Some(input),
        };
        let result = |id: &str, output: &str| Part::ToolResult {
            name: "tool".into(),
            id: Some(id.into()),
            output: Some(output.into()),
            is_error: None,
        };
        let message = |role, parts, summary| Message {
            role,
            parts,
            time_created: 1000,
            origin: Some(Agent::Cline),
            model: None,
            summary,
        };
        let diff = "<<<<<<< SEARCH\n:start_line:3\n-------\nold\n=======\nnew\n>>>>>>> REPLACE";
        let mut session = session_in(
            "/repo",
            vec![
                message(
                    Role::Assistant,
                    vec![
                        call("execute_command", "c1", serde_json::json!({ "command": "ls", "cwd": "sub" })),
                        call("read_file", "c2", serde_json::json!({ "args": { "file": { "path": "../x/a.rs" } } })),
                        call("apply_diff", "c3", serde_json::json!({ "path": "a.rs", "diff": diff })),
                        call("update_todo_list", "c4", serde_json::json!({ "todos": "[x] one\n[-] two\n[ ] three" })),
                        call("ask_followup_question", "c5", serde_json::json!({ "question": "Which?", "follow_up": "<suggest>A</suggest>\n<suggest>B</suggest>" })),
                        call("new_task", "c6", serde_json::json!({ "mode": "code", "message": "Do the sub thing\nin detail" })),
                        call("use_mcp_tool", "c7", serde_json::json!({ "server_name": "dh.serper", "tool_name": "search", "arguments": "{\"q\":1}" })),
                        call("browser_action", "c8", serde_json::json!({ "action": "launch" })),
                    ],
                    false,
                ),
                message(
                    Role::User,
                    vec![result("c1", "a.rs"), result("c5", "<answer>\nB\n</answer>")],
                    false,
                ),
                message(Role::Assistant, vec![Part::text("## Summary")], true),
            ],
        );
        session.origin = Agent::Cline;
        session.parent = Some("parent-task".into());
        session.children = vec!["child-task".into()];
        let export = write_export(&session, "cline-tools");

        let parts = &export["messages"][0]["parts"];
        let tool = |i: usize| (parts[i]["tool"].as_str().unwrap(), &parts[i]["state"]);
        assert_eq!(tool(0).0, "bash");
        assert_eq!(tool(0).1["input"], serde_json::json!({ "command": "ls", "workdir": "/repo/sub" }));
        assert_eq!(tool(0).1["metadata"]["output"], "a.rs");
        assert_eq!(tool(1).0, "read");
        assert_eq!(tool(1).1["input"]["filePath"], "/x/a.rs");
        assert_eq!(tool(2).0, "edit");
        assert_eq!(
            tool(2).1["input"],
            serde_json::json!({ "filePath": "/repo/a.rs", "oldString": "old", "newString": "new" })
        );
        assert!(tool(2).1["metadata"]["diff"].as_str().unwrap().ends_with("@@ -3,1 +3,1 @@\n-old\n+new\n"));
        assert_eq!(tool(3).0, "todowrite");
        assert_eq!(tool(3).1["metadata"]["todos"][1], serde_json::json!({ "content": "two", "status": "in_progress" }));
        assert_eq!(tool(4).0, "question");
        assert_eq!(tool(4).1["input"]["questions"][0]["options"][1]["label"], "B");
        assert_eq!(tool(4).1["metadata"]["answers"], serde_json::json!([["B"]]));
        assert_eq!(tool(5).0, "task");
        assert_eq!(tool(5).1["input"]["description"], "Do the sub thing");
        assert_eq!(tool(5).1["metadata"]["sessionId"], IdGen::session_id("cline:child-task"));
        assert_eq!(tool(6).0, "dh_serper_search");
        assert_eq!(tool(6).1["input"], serde_json::json!({ "q": 1 }));
        assert_eq!(tool(7).0, "browser_action", "no opencode equivalent: left as is");

        assert_eq!(export["info"]["parentID"], IdGen::session_id("cline:parent-task"));
        let msgs = export["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "the result-only message folds away; the summary adds one");
        let (compaction, summary) = (&msgs[1], &msgs[2]);
        assert_eq!(compaction["info"]["role"], "user");
        assert_eq!(compaction["parts"][0]["type"], "compaction");
        assert_eq!(summary["info"]["summary"], true);
        assert_eq!(summary["info"]["finish"], "stop");
        assert_eq!(
            msgs[0]["info"]["finish"], "tool-calls",
            "a reply that ends calling tools is finished, so opencode checks its size"
        );
        assert_eq!(summary["info"]["parentID"], compaction["info"]["id"]);
        assert!(compaction["info"]["id"].as_str() < summary["info"]["id"].as_str());
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
///
/// The 14 trailing characters are derived from `seed` and the emission count
/// rather than drawn at random, so converting the same source session again
/// yields the same ids, and `opencode import` (which skips rows it already has)
/// leaves an earlier import untouched instead of duplicating every message.
struct IdGen {
    last_timestamp: i64,
    counter: u64,
    /// Import-time "now". Generated ids are clamped to sort at or below the id a live opencode would
    /// mint at this instant, so an imported message can never outrank the live conversation when the
    /// session is resumed (resume happens strictly after import). See `next`.
    now_ceiling: i64,
    seed: String,
    emitted: u64,
}

impl IdGen {
    const CHARS: &'static [u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    /// opencode's 12-hex id prefix encodes only the low 48 bits of `timestamp_ms * 0x1000 + counter`,
    /// i.e. `timestamp_ms mod 2^36`, so the sortable id space WRAPS every `1 << 36` ms (~795 days).
    /// An id built from a timestamp in an earlier window sorts ABOVE a fresh native id — which is
    /// exactly what re-triggers the run-loop hang on opencode <= 1.18.4 (upstream `dev` avoids it by
    /// comparing `time.created` first). We clamp every imported timestamp into the single window that
    /// ends at `now_ceiling`, so it can never wrap back above a resume-time native id.
    const WRAP_MS: i64 = 1 << 36;

    fn new(now_ceiling: i64, seed: &str) -> Self {
        Self { last_timestamp: -1, counter: 0, now_ceiling, seed: seed.to_string(), emitted: 0 }
    }

    /// 14 base62 characters determined by `name` alone.
    fn stable_chars(name: &str) -> String {
        let a = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes());
        let b = Uuid::new_v5(&a, b"baton");
        a.as_bytes()
            .iter()
            .chain(b.as_bytes())
            .take(14)
            .map(|b| Self::CHARS[(*b as usize) % 62] as char)
            .collect()
    }

    /// opencode session id for a source session, stable across conversions, so
    /// a session can be referenced (as a subtask's parent, say) from another
    /// conversion without knowing anything about it but its source id.
    fn session_id(seed: &str) -> String {
        let hash = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("session:{seed}").as_bytes());
        let hex: String = hash.as_bytes()[..6].iter().map(|b| format!("{b:02x}")).collect();
        format!("ses_{hex}{}", Self::stable_chars(&format!("session:{seed}")))
    }

    /// opencode's 12-hex, time-sortable prefix for an already-combined `timestamp_ms * 0x1000 + counter`.
    fn encode_prefix(current: u128) -> String {
        let mut s = String::with_capacity(12);
        for i in 0..6 {
            s.push_str(&format!("{:02x}", ((current >> (40 - 8 * i)) & 0xff) as u8));
        }
        s
    }

    /// `<prefix>_<12 hex time chars><14 random chars>`; monotonic for equal or
    /// out-of-order timestamps, so ordering always follows emission order.
    fn next(&mut self, prefix: &str, timestamp_ms: i64) -> String {
        // Clamp into the single ~795-day wrap window ending at `now_ceiling` (import time): never
        // above the ceiling, and never before the window's start (`now_ceiling` rounded down to a
        // 2^36-ms boundary). Within that window `timestamp mod 2^36` is monotonic and always <= the
        // ceiling's, so a native id opencode mints at resume (strictly later than import) outranks
        // every id we emit. Timestamps older than the window collapse to its start but keep their
        // emission order via the monotonic counter below. LIMITATION: a session spanning >~795 days,
        // or a resume that itself crosses a wrap boundary, can't be fully ordered by id alone.
        let window_start = self.now_ceiling - self.now_ceiling.rem_euclid(Self::WRAP_MS);
        let clamped = timestamp_ms.clamp(window_start, self.now_ceiling);
        // Never let a stale timestamp produce a smaller id than the previous one.
        let ts = clamped.max(self.last_timestamp);
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
        out.push_str(&Self::encode_prefix(current));
        self.emitted += 1;
        out.push_str(&Self::stable_chars(&format!("{}:{prefix}:{}", self.seed, self.emitted)));
        out
    }

    /// A fresh native opencode-style id (counter 0) as a live instance would mint at `timestamp_ms`.
    /// Test-only: lets ordering assertions use a real native id instead of the wall clock, so they
    /// stay stable across opencode's ~795-day id-wrap boundary.
    #[cfg(test)]
    fn native_like(prefix: &str, timestamp_ms: i64) -> String {
        format!("{prefix}_{}{}", Self::encode_prefix((timestamp_ms as u128) * 0x1000), "0".repeat(14))
    }
}

#[cfg(test)]
mod idgen_tests {
    use super::IdGen;

    #[test]
    fn ids_sort_chronologically_and_monotonically() {
        // Ceiling above the timestamps used, so the wrap-clamp doesn't interfere with this test.
        let mut g = IdGen::new(1_700_000_002_000, "test");
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
    fn imported_ids_never_outrank_a_native_id_minted_at_resume() {
        // Date-INDEPENDENT: opencode's id prefix carries only `timestamp mod 2^36`, so it wraps every
        // ~795 days; comparing against `Utc::now()` made this test fail for ~7.5 months of every
        // 26-month cycle (a boundary was crossed 2026-08-14). Instead we fix a reference "now"
        // mid-window and compare against a real native-style id built with the same 48-bit truncation.
        const WRAP: i64 = 1 << 36;
        let now = 100 * WRAP + WRAP / 2; // mid wrap-window, far from any boundary
        let mut g = IdGen::new(now, "test");
        // An id from a PREVIOUS wrap window (which naively sorts ABOVE `now`) must be clamped below it.
        let ancient = g.next("msg", now - 3 * WRAP - 5_000);
        let recent = g.next("msg", now - 60_000); // within the current window
        // A fresh native id a live opencode would mint at resume (>= import time), counter 0.
        let native = IdGen::native_like("msg", now);
        assert!(recent < native, "recent import must sort below a resume-time native id: {recent} !< {native}");
        assert!(ancient < native, "a wrapped-old import must still sort below native: {ancient} !< {native}");
        assert!(ancient < recent, "emission order preserved across the clamp: {ancient} !< {recent}");
    }
}
