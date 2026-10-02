//! Cline-family session format: Cline, Roo Code, Kilo Code and Zoo Code.
//!
//! Each task lives in `<globalStorage>/<extension-id>/tasks/<task-id>/`:
//!   - `api_conversation_history.json`: the Anthropic-shaped `MessageParam`
//!     array the agent sent to the model (`{ role, content, ts? }`). Roo-derived
//!     agents add `ts`, and `isSummary` on context-condensing summaries.
//!   - `ui_messages.json`: the chat as shown, with timestamps. Cline's api
//!     history has none, so its times come from here, matched through
//!     `conversationHistoryIndex`.
//!   - `history_item.json` (Roo): workspace, title, time, parent/root task.
//!   - `task_metadata.json` (Cline): the models each request used.
//!
//! Older Roo and the task index keep the history item in `tasks/_index.json`;
//! Cline keeps its own in `<extension-id>/state/taskHistory.json`.
//!
//! Tools come in two protocols. Native: `tool_use` / `tool_result` blocks.
//! XML: the call is XML in the assistant's text (`<read_file><path>a</path>
//! </read_file>`), and the next user message starts its result with a
//! `[read_file for 'a'] Result:` text block. Both become tool calls with
//! results. Every user message also carries an `<environment_details>` block
//! the agent appended (open tabs, time, workspace, model); it is read for the
//! workspace and model and then dropped.
//!
//! The global storage path varies by platform:
//!   macOS:  ~/Library/Application Support/Code/User/globalStorage/<extension-id>
//!   Linux:  ~/.config/Code/User/globalStorage/<extension-id>
//!   Windows: %APPDATA%\Code\User\globalStorage\<extension-id>

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::canonical::{Agent, Format, Message, ModelRef, Part, Role, Session, SessionRef};

pub struct Cline;

const HISTORY_FILE: &str = "api_conversation_history.json";

/// Extension ids of the Cline family, in the order `session_dir` prefers them.
const EXTENSIONS: &[&str] = &[
    "saoudrizwan.claude-dev",
    "rooveterinaryinc.roo-cline",
    "kilocode.kilo-code",
    "zoocodeorganization.zoo-code",
];

/// Tools the XML protocol can call; a tag is only parsed as a call when it is
/// one of these, so ordinary XML or HTML in a reply stays text.
const XML_TOOLS: &[&str] = &[
    "access_mcp_resource",
    "apply_diff",
    "ask_followup_question",
    "attempt_completion",
    "browser_action",
    "codebase_search",
    "condense",
    "delete_file",
    "edit_file",
    "execute_command",
    "fetch_instructions",
    "generate_image",
    "insert_content",
    "list_code_definition_names",
    "list_files",
    "load_mcp_documentation",
    "new_rule",
    "new_task",
    "plan_mode_respond",
    "plan_mode_response",
    "read_file",
    "replace_in_file",
    "report_bug",
    "run_slash_command",
    "search_and_replace",
    "search_files",
    "switch_mode",
    "update_todo_list",
    "use_mcp_tool",
    "web_fetch",
    "write_to_file",
];

/// Tools whose call is the agent talking to the user: they become plain text,
/// and so does the user's reply to them.
const SPEECH_TOOLS: &[&str] = &["attempt_completion", "plan_mode_respond", "plan_mode_response"];

/// Parameters whose value is file content, kept verbatim rather than trimmed.
const VERBATIM_PARAMS: &[&str] = &["content", "diff", "result", "response"];

impl Format for Cline {
    const AGENT: Agent = Agent::Cline;
    const NAME: &'static str = "Cline / Roo Code / Kilo Code / Zoo Code";

    fn session_dir() -> PathBuf {
        let Some(base) = global_storage() else {
            return PathBuf::from(".cline");
        };
        EXTENSIONS
            .iter()
            .map(|ext| base.join(ext).join("tasks"))
            .find(|dir| dir.exists())
            .unwrap_or_else(|| base.join(EXTENSIONS[0]).join("tasks"))
    }

    fn list() -> Vec<SessionRef> {
        let Some(base) = global_storage() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ext in EXTENSIONS {
            let Ok(tasks) = std::fs::read_dir(base.join(ext).join("tasks")) else {
                continue;
            };
            for task in tasks.flatten() {
                let path = task.path().join(HISTORY_FILE);
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                out.push(SessionRef {
                    agent: Agent::Cline,
                    id: task.file_name().to_string_lossy().into_owned(),
                    title: String::new(),
                    path,
                    mtime,
                });
            }
        }
        out
    }

    /// `path` is a task directory or the `api_conversation_history.json` in one.
    fn read(path: &Path) -> anyhow::Result<Session> {
        let history_path = if path.is_dir() { path.join(HISTORY_FILE) } else { path.to_path_buf() };
        let task_dir = history_path.parent().unwrap_or(Path::new("."));
        let raw = std::fs::read_to_string(&history_path)
            .with_context(|| format!("reading cline task history {}", history_path.display()))?;
        let history: Vec<ApiMessage> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing cline task history {}", history_path.display()))?;
        let task_id = task_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let item = HistoryItem::find(task_dir, &task_id);
        let times = message_times(task_dir, &history, &item, &task_id);
        let usage = model_usage(task_dir);

        let mut reader = Reader::default();
        let mut messages = Vec::new();
        for (msg, ts) in history.iter().zip(times) {
            let role = match msg.role.as_str() {
                "assistant" => Role::Assistant,
                "user" => Role::User,
                _ => continue,
            };
            let blocks = blocks(&msg.content);
            let parts = match role {
                Role::Assistant => reader.assistant_parts(&blocks),
                _ => reader.user_parts(&blocks),
            };
            if parts.is_empty() {
                continue;
            }
            let model = match role {
                Role::Assistant => usage_at(&usage, ts).or_else(|| reader.model.clone()),
                _ => None,
            };
            messages.push(Message {
                role,
                parts,
                time_created: ts,
                origin: Some(Agent::Cline),
                model,
                summary: msg.is_summary,
            });
        }

        let title = item
            .task
            .as_deref()
            .and_then(title_line)
            .or_else(|| {
                messages
                    .iter()
                    .find(|m| m.role == Role::User)
                    .and_then(|m| m.parts.iter().find_map(Part::as_text))
                    .and_then(title_line)
            })
            .unwrap_or_else(|| "Cline task".to_string());
        let time_created = messages.first().map(|m| m.time_created).or(item.ts).unwrap_or(0);
        let time_updated = messages
            .last()
            .map(|m| m.time_created)
            .into_iter()
            .chain(item.ts)
            .max()
            .unwrap_or(time_created);
        let parent = item
            .parent_task_id
            .clone()
            .filter(|p| task_dir.parent().is_some_and(|tasks| tasks.join(p).is_dir()));
        let children = if reader.spawned_subtasks {
            subtasks(task_dir, &task_id)
        } else {
            Vec::new()
        };

        Ok(Session {
            source_id: task_id,
            origin: Agent::Cline,
            title,
            time_created,
            time_updated,
            directory: item.workspace.clone().or(reader.workspace),
            title_prefix: None,
            parent,
            children,
            messages,
        })
    }

    fn write(_session: &Session, _path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cline write not implemented yet.")
    }
}

fn global_storage() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library").join("Application Support"))
    } else {
        dirs::config_dir()
    };
    base.map(|b| b.join("Code").join("User").join("globalStorage"))
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    role: String,
    #[serde(default)]
    content: serde_json::Value,
    #[serde(default)]
    ts: Option<i64>,
    #[serde(default, rename = "isSummary")]
    is_summary: bool,
}

/// What the agent's own task list records about a task.
#[derive(Debug, Default, Clone)]
struct HistoryItem {
    task: Option<String>,
    ts: Option<i64>,
    workspace: Option<String>,
    parent_task_id: Option<String>,
}

impl HistoryItem {
    /// Merges every record of the task: Roo's `history_item.json`, the
    /// `tasks/_index.json` index, then Cline's `state/taskHistory.json`. The
    /// first source to know a field wins.
    fn find(task_dir: &Path, task_id: &str) -> Self {
        let tasks = task_dir.parent();
        let extension = tasks.and_then(Path::parent);
        let mut records: Vec<serde_json::Value> = Vec::new();
        if let Some(item) = read_json(&task_dir.join("history_item.json")) {
            records.push(item);
        }
        let matching = |list: Option<serde_json::Value>| {
            list.and_then(|l| l.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.get("id").and_then(|i| i.as_str()) == Some(task_id))
        };
        if let Some(tasks) = tasks {
            let index = read_json(&tasks.join("_index.json")).and_then(|i| i.get("entries").cloned());
            records.extend(matching(index));
        }
        if let Some(extension) = extension {
            records.extend(matching(read_json(&extension.join("state").join("taskHistory.json"))));
        }
        let first = |keys: &[&str]| {
            records.iter().find_map(|r| {
                keys.iter()
                    .find_map(|k| r.get(*k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
                    .map(str::to_string)
            })
        };
        Self {
            task: first(&["task"]),
            ts: records.iter().find_map(|r| r.get("ts").and_then(|t| t.as_i64())),
            workspace: first(&["workspace", "cwdOnTaskInitialization", "shadowGitConfigWorkTree"])
                .filter(|w| looks_absolute(w)),
            parent_task_id: first(&["parentTaskId"]),
        }
    }
}

/// The first JSON value in a file. A few agent files carry trailing garbage
/// from interrupted writes; what precedes it is still good.
fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::Deserializer::from_str(&raw)
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()
}

fn looks_absolute(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

/// The first non-empty line of a task's opening prompt, capped for a title.
fn title_line(text: &str) -> Option<String> {
    let is_tag = |l: &str| l.starts_with('<') && l.ends_with('>') && !l.contains(' ');
    user_text(text)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !is_tag(l))
        .map(|l| l.chars().take(100).collect())
}

/// When each api message was sent. Roo-derived agents stamp `ts` on every
/// message. Cline does not; its ui messages carry `conversationHistoryIndex`,
/// the index of the last api message at the time, so api message `j` began
/// with the first ui message whose index is `j - 1`. Gaps take the previous
/// time, and the result never runs backwards.
fn message_times(task_dir: &Path, history: &[ApiMessage], item: &HistoryItem, task_id: &str) -> Vec<i64> {
    let mut ui_times: Vec<Option<i64>> = vec![None; history.len()];
    if history.iter().any(|m| m.ts.is_none())
        && let Some(serde_json::Value::Array(ui)) = read_json(&task_dir.join("ui_messages.json"))
    {
        for m in ui {
            let (Some(ts), Some(index)) = (
                m.get("ts").and_then(|t| t.as_i64()),
                m.get("conversationHistoryIndex").and_then(|i| i.as_i64()),
            ) else {
                continue;
            };
            if let Some(slot) = usize::try_from(index + 1).ok().and_then(|j| ui_times.get_mut(j)) {
                *slot = Some(slot.map_or(ts, |t| t.min(ts)));
            }
        }
    }
    // A Cline task id is its creation time in epoch milliseconds.
    let start = item.ts.or_else(|| task_id.parse().ok()).unwrap_or(0);
    let mut last = 0;
    history
        .iter()
        .zip(ui_times)
        .enumerate()
        .map(|(i, (m, ui))| {
            let ts = m.ts.or(ui).unwrap_or(if i == 0 { start } else { last });
            last = ts.max(last);
            last
        })
        .collect()
}

/// `(time, model)` of every request Cline recorded in `task_metadata.json`.
fn model_usage(task_dir: &Path) -> Vec<(i64, ModelRef)> {
    let Some(meta) = read_json(&task_dir.join("task_metadata.json")) else {
        return Vec::new();
    };
    let mut usage: Vec<(i64, ModelRef)> = meta
        .get("model_usage")
        .and_then(|u| u.as_array())
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let ts = u.get("ts")?.as_i64()?;
            let id = u.get("model_id")?.as_str()?;
            let provider = u.get("model_provider_id").and_then(|p| p.as_str());
            Some((ts, model_ref(id, provider)?))
        })
        .collect();
    usage.sort_by_key(|(ts, _)| *ts);
    usage
}

/// The model of the last recorded request at or before `ts`.
fn usage_at(usage: &[(i64, ModelRef)], ts: i64) -> Option<ModelRef> {
    usage
        .iter()
        .take_while(|(t, _)| *t <= ts)
        .last()
        .or(usage.first())
        .map(|(_, m)| m.clone())
}

/// A model id as recorded by the agent, with the provider it names or, failing
/// that, the provider its name implies. Roo's `:thinking` suffix selects a
/// variant of the same model.
fn model_ref(id: &str, provider: Option<&str>) -> Option<ModelRef> {
    let id = id.trim();
    // Roo selects variants of a model with a suffix (`:thinking`, `:1m`); an
    // OpenRouter id keeps its own (`:free`).
    let id = if id.contains('/') { id } else { id.split(':').next().unwrap_or(id) };
    if id.is_empty() {
        return None;
    }
    let provider = match provider {
        Some("gemini") => "google",
        Some("openai-native") => "openai",
        Some("bedrock") => "amazon-bedrock",
        Some(p) if !p.is_empty() => p,
        _ if id.contains('/') => "openrouter",
        _ if id.starts_with("claude") => "anthropic",
        _ if id.starts_with("gemini") => "google",
        _ if ["gpt", "o1", "o3", "o4", "chatgpt"].iter().any(|p| id.starts_with(p)) => "openai",
        _ if id.starts_with("grok") => "xai",
        _ if id.starts_with("deepseek") => "deepseek",
        _ => return None,
    };
    Some(ModelRef {
        provider: provider.to_string(),
        id: id.to_string(),
    })
}

/// Source ids of the tasks spawned from `task_id` (their history item names it
/// as parent), oldest first.
fn subtasks(task_dir: &Path, task_id: &str) -> Vec<String> {
    let Some(Ok(tasks)) = task_dir.parent().map(std::fs::read_dir) else {
        return Vec::new();
    };
    let mut children: Vec<(i64, String)> = tasks
        .flatten()
        .filter_map(|entry| {
            let item = read_json(&entry.path().join("history_item.json"))?;
            (item.get("parentTaskId")?.as_str()? == task_id).then(|| {
                let ts = item.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
                (ts, entry.file_name().to_string_lossy().into_owned())
            })
        })
        .collect();
    children.sort();
    children.into_iter().map(|(_, id)| id).collect()
}

fn blocks(content: &serde_json::Value) -> Vec<serde_json::Value> {
    match content {
        serde_json::Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
        serde_json::Value::Array(arr) => arr.clone(),
        _ => Vec::new(),
    }
}

/// State carried across messages while reading one task.
#[derive(Default)]
struct Reader {
    /// XML-protocol calls awaiting their result, as `(tool, call id)`.
    pending: VecDeque<(String, String)>,
    /// Call ids of speech tools turned into text; their results are user text.
    spoken: HashSet<String>,
    /// Model named by the latest environment details, which the next reply uses.
    model: Option<ModelRef>,
    workspace: Option<String>,
    spawned_subtasks: bool,
    xml_calls: usize,
}

/// A result being collected from an XML-protocol user message.
struct OpenResult {
    tool: String,
    text: Vec<String>,
    images: Vec<Part>,
}

impl Reader {
    fn assistant_parts(&mut self, blocks: &[serde_json::Value]) -> Vec<Part> {
        let native_tools = blocks.iter().any(|b| block_type(b) == "tool_use");
        let signed: HashSet<&str> = blocks
            .iter()
            .filter(|b| block_type(b) == "thinking")
            .filter_map(|b| b.get("thinking").and_then(|t| t.as_str()))
            .collect();
        let mut parts = Vec::new();
        for block in blocks {
            match block_type(block) {
                "text" => {
                    let text = block_text(block);
                    if native_tools {
                        push_text(&mut parts, text);
                    } else {
                        self.parse_xml_text(text, &mut parts);
                    }
                }
                "reasoning" => {
                    let text = block_text(block);
                    if !text.trim().is_empty() && !signed.contains(text) {
                        parts.push(Part::Reasoning { text: text.to_string(), signature: None });
                    }
                }
                "thinking" => {
                    let text = block.get("thinking").and_then(|t| t.as_str()).unwrap_or_default();
                    let signature = block.get("signature").and_then(|s| s.as_str()).map(str::to_string);
                    if !text.trim().is_empty() || signature.is_some() {
                        parts.push(Part::Reasoning { text: text.to_string(), signature });
                    }
                }
                "tool_use" => {
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("tool").to_string();
                    let id = block.get("id").and_then(|v| v.as_str()).map(str::to_string);
                    let input = block.get("input").cloned().unwrap_or_else(|| serde_json::json!({}));
                    self.push_call(&mut parts, name, id, input);
                }
                "image" => parts.extend(image_part(block)),
                _ => {}
            }
        }
        parts
    }

    fn push_call(&mut self, parts: &mut Vec<Part>, name: String, id: Option<String>, input: serde_json::Value) {
        if SPEECH_TOOLS.contains(&name.as_str()) {
            let said = ["result", "response"]
                .iter()
                .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
                .unwrap_or_default();
            push_text(parts, said);
            if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
                push_text(parts, &format!("`{command}`"));
            }
            self.spoken.extend(id);
            return;
        }
        if name == "new_task" {
            self.spawned_subtasks = true;
        }
        parts.push(Part::ToolCall { name, id, input: Some(input) });
    }

    /// Split an XML-protocol reply into text, `<thinking>` reasoning and tool
    /// calls. A call is a known tool tag opening at the start of a line with a
    /// matching close; anything else, including an unterminated call from a
    /// cut-off reply, stays text.
    fn parse_xml_text(&mut self, text: &str, parts: &mut Vec<Part>) {
        let mut rest = text;
        while let Some((start, tag)) = next_tag(rest) {
            let close = format!("</{tag}>");
            let body_start = start + tag.len() + 2;
            push_text(parts, &rest[..start]);
            let (body, after) = match rest[body_start..].find(&close) {
                Some(len) => (&rest[body_start..body_start + len], &rest[body_start + len + close.len()..]),
                None if tag == "thinking" => (&rest[body_start..], ""),
                None => {
                    // The agent ran a call the reply never closed: closed with
                    // another tool's tag, or cut off by an interruption, which
                    // the agent noted after it. Take the call to the end.
                    let (body, notice) = split_interruption(&rest[body_start..]);
                    let body = body.trim_end();
                    let body = XML_TOOLS
                        .iter()
                        .find_map(|t| body.strip_suffix(&format!("</{t}>")))
                        .unwrap_or(body);
                    (body, notice)
                }
            };
            if tag == "thinking" {
                if !body.trim().is_empty() {
                    parts.push(Part::Reasoning { text: body.trim().to_string(), signature: None });
                }
            } else {
                self.xml_calls += 1;
                let id = format!("xmlcall_{}", self.xml_calls);
                self.pending.push_back((tag.to_string(), id.clone()));
                self.push_call(parts, tag.to_string(), Some(id), xml_params(body));
            }
            rest = after;
        }
        push_text(parts, rest);
    }

    fn user_parts(&mut self, blocks: &[serde_json::Value]) -> Vec<Part> {
        let mut parts = Vec::new();
        let mut open: Option<OpenResult> = None;
        for block in blocks {
            match block_type(block) {
                "text" => {
                    let (text, environment) = split_environment(block_text(block));
                    if let Some(environment) = environment {
                        self.read_environment(environment);
                    }
                    if text.trim().is_empty() {
                    } else if let Some((tool, rest)) = result_header(text) {
                        self.close_result(open.take(), &mut parts);
                        open = Some(OpenResult { tool, text: vec![rest.to_string()], images: Vec::new() });
                    } else if let Some(tool) = not_executed(text) {
                        match self.take_pending(&tool) {
                            Some(id) if !self.spoken.contains(&id) => parts.push(Part::ToolResult {
                                name: tool,
                                id: Some(id),
                                output: Some(text.to_string()),
                                is_error: Some(true),
                            }),
                            _ => push_text(&mut parts, text),
                        }
                    } else if let Some(result) = &mut open {
                        result.text.push(text.to_string());
                    } else {
                        push_text(&mut parts, &user_text(text));
                    }
                }
                "image" => match &mut open {
                    Some(result) => result.images.extend(image_part(block)),
                    None => parts.extend(image_part(block)),
                },
                "tool_result" => {
                    self.close_result(open.take(), &mut parts);
                    self.native_result(block, &mut parts);
                }
                _ => {}
            }
        }
        self.close_result(open.take(), &mut parts);
        parts
    }

    fn native_result(&mut self, block: &serde_json::Value, parts: &mut Vec<Part>) {
        let id = block.get("tool_use_id").and_then(|v| v.as_str()).map(str::to_string);
        let mut text = Vec::new();
        let mut images = Vec::new();
        match block.get("content") {
            Some(serde_json::Value::String(s)) => text.push(s.clone()),
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    match block_type(item) {
                        "text" => text.push(block_text(item).to_string()),
                        "image" => images.extend(image_part(item)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let output = text.join("\n");
        if id.as_ref().is_some_and(|id| self.spoken.contains(id)) {
            push_text(parts, &user_text(&output));
            parts.extend(images);
            return;
        }
        parts.push(Part::ToolResult {
            name: "tool".into(),
            id,
            output: Some(output),
            is_error: block.get("is_error").and_then(|e| e.as_bool()),
        });
        parts.extend(images);
    }

    fn close_result(&mut self, result: Option<OpenResult>, parts: &mut Vec<Part>) {
        let Some(result) = result else { return };
        let output = result
            .text
            .iter()
            .map(|t| t.trim_matches('\n'))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        match self.take_pending(&result.tool) {
            _ if SPEECH_TOOLS.contains(&result.tool.as_str()) => push_text(parts, &user_text(&output)),
            Some(id) => parts.push(Part::ToolResult {
                name: result.tool,
                id: Some(id),
                output: Some(output),
                is_error: None,
            }),
            None => push_text(parts, &format!("[{}] Result:\n{output}", result.tool)),
        }
        parts.extend(result.images);
    }

    /// The oldest unanswered XML call to `tool`. Calls answered out of order or
    /// never answered are left behind; the writer gives those an empty result.
    fn take_pending(&mut self, tool: &str) -> Option<String> {
        let at = self.pending.iter().position(|(name, _)| name == tool)?;
        self.pending.remove(at).map(|(_, id)| id)
    }

    /// The workspace and model from an `<environment_details>` block.
    fn read_environment(&mut self, text: &str) {
        if self.workspace.is_none() {
            self.workspace = ["# Current Workspace Directory (", "# Current Working Directory ("]
                .iter()
                .find_map(|marker| {
                    let after = &text[text.find(marker)? + marker.len()..];
                    let dir = &after[..after.find(") Files")?];
                    looks_absolute(dir).then(|| dir.to_string())
                });
        }
        if let Some(model) = text
            .find("<model>")
            .and_then(|at| {
                let after = &text[at + "<model>".len()..];
                Some(&after[..after.find("</model>")?])
            })
            .and_then(|id| model_ref(id, None))
        {
            self.model = Some(model);
        }
    }
}

/// The earliest known tool or `<thinking>` tag opening at the start of a line
/// in `text`, as `(byte offset, tag)`.
fn next_tag(text: &str) -> Option<(usize, &'static str)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(name) = trimmed.strip_prefix('<').and_then(|t| t.split_once('>')).map(|(n, _)| n)
            && let Some(tag) = XML_TOOLS.iter().chain(&["thinking"]).find(|t| **t == name)
        {
            return Some((offset + indent, tag));
        }
        offset += line.len();
    }
    None
}

/// `text` split before a trailing `[Response interrupted by ...]` notice.
fn split_interruption(text: &str) -> (&str, &str) {
    match text.rfind("[Response interrupted by") {
        Some(at) if text[at..].trim_end().ends_with(']') => (&text[..at], &text[at..]),
        _ => (text, ""),
    }
}

/// The `<param>value</param>` children of an XML tool call as a JSON object.
/// `args` and `file` nest (Roo's multi-file `read_file` and `apply_diff`), as
/// does a `diff` holding `<content>`; a repeated child becomes an array. An
/// unclosed last child, from a cut-off call, takes the rest of the body.
fn xml_params(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut rest = body;
    loop {
        rest = rest.trim_start();
        let Some(name) = rest
            .strip_prefix('<')
            .and_then(|r| r.split_once('>'))
            .map(|(n, _)| n)
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        else {
            break;
        };
        let close = format!("</{name}>");
        let inner_start = name.len() + 2;
        let found = if VERBATIM_PARAMS.contains(&name) {
            rest.rfind(&close)
        } else {
            rest[inner_start..].find(&close).map(|i| i + inner_start)
        };
        let end = found.filter(|e| *e >= inner_start).unwrap_or(rest.len());
        let raw = &rest[inner_start..end];
        let value = if matches!(name, "args" | "file")
            || (name == "diff" && raw.trim_start().starts_with("<content>"))
        {
            xml_params(raw)
        } else if VERBATIM_PARAMS.contains(&name) {
            let raw = raw.strip_prefix('\n').unwrap_or(raw);
            serde_json::json!(raw.strip_suffix('\n').unwrap_or(raw))
        } else {
            serde_json::json!(raw.trim())
        };
        match map.get_mut(name) {
            Some(serde_json::Value::Array(items)) => items.push(value),
            Some(existing) => *existing = serde_json::json!([existing.take(), value]),
            None => {
                map.insert(name.to_string(), value);
            }
        }
        rest = rest.get(end + close.len()..).unwrap_or_default();
    }
    serde_json::Value::Object(map)
}

/// `text` split before the `<environment_details>` block the agent appended to
/// it. Usually that block is a text block of its own; sometimes it is glued to
/// the end of a tool result.
fn split_environment(text: &str) -> (&str, Option<&str>) {
    const OPEN: &str = "<environment_details>\n";
    if !text.trim_end().ends_with("</environment_details>") {
        return (text, None);
    }
    match text.rfind(OPEN) {
        Some(at) => (&text[..at], Some(&text[at..])),
        None => (text, None),
    }
}

/// `[tool for 'x'] Result:` (or `[tool] Result:`) at the start of an
/// XML-protocol result, as `(tool, text after the header)`.
fn result_header(text: &str) -> Option<(String, &str)> {
    let rest = text.strip_prefix('[')?;
    let tool: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    if tool.is_empty() {
        return None;
    }
    let after_name = &rest[tool.len()..];
    if !(after_name.starts_with(']') || after_name.starts_with(" for ") || after_name.starts_with(". ")) {
        return None;
    }
    let header_end = text.find("] Result:")? + "] Result:".len();
    Some((tool, &text[header_end..]))
}

/// The tool named by `Tool [x] was not executed because ...`.
fn not_executed(text: &str) -> Option<String> {
    let rest = text.strip_prefix("Tool [")?;
    let (tool, after) = rest.split_once(']')?;
    after.starts_with(" was not executed").then(|| tool.to_string())
}

/// What the user actually wrote, without the wrappers the agent put around it
/// (`<task>`, `<feedback>`, `<user_message>`, `<answer>`) and the boilerplate
/// around a reply to a completion or a resumed task.
fn user_text(text: &str) -> String {
    let trimmed = text.trim();
    for tag in ["user_message", "feedback", "answer"] {
        let values = inner_values(trimmed, tag);
        if !values.is_empty() {
            return values.join("\n\n");
        }
    }
    unwrap_tag(trimmed, "task").unwrap_or(trimmed).to_string()
}

fn inner_values<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.rfind(&close) else { break };
        out.push(after[..end].trim());
        rest = &after[end + close.len()..];
    }
    out
}

/// `text` without an outer `<tag>...</tag>` wrapping all of it.
fn unwrap_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    text.strip_prefix(&format!("<{tag}>"))?
        .strip_suffix(&format!("</{tag}>"))
        .map(str::trim)
}

/// Appended by Roo when it cut a reply off after its first tool call.
const INTERRUPTED_BY_TOOL: &str = "[Response interrupted by a tool use result. Only one tool may be used at a time and should be placed at the end of the message.]";

fn push_text(parts: &mut Vec<Part>, text: &str) {
    let text = text.replace(INTERRUPTED_BY_TOOL, "");
    let text = text.trim();
    if !text.is_empty() {
        parts.push(Part::Text { text: text.to_string() });
    }
}

fn block_type(block: &serde_json::Value) -> &str {
    block.get("type").and_then(|t| t.as_str()).unwrap_or_default()
}

fn block_text(block: &serde_json::Value) -> &str {
    block.get("text").and_then(|t| t.as_str()).unwrap_or_default()
}

fn image_part(block: &serde_json::Value) -> Option<Part> {
    let source = block.get("source")?;
    let data = source.get("data")?.as_str()?;
    Some(Part::Attachment {
        mime: source
            .get("media_type")
            .and_then(|m| m.as_str())
            .unwrap_or("image/png")
            .to_string(),
        path: None,
        data: Some(data.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch `<extension>/tasks/` directory, removed on drop.
    struct Storage(PathBuf);

    impl Storage {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("baton-cline-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(root.join("tasks")).unwrap();
            Self(root)
        }

        fn task(&self, id: &str, files: &[(&str, serde_json::Value)]) -> PathBuf {
            let dir = self.0.join("tasks").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, value) in files {
                std::fs::write(dir.join(name), value.to_string()).unwrap();
            }
            dir
        }
    }

    impl Drop for Storage {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn env(workspace: &str, model: &str) -> serde_json::Value {
        json!({ "type": "text", "text": format!(
            "<environment_details>\n# Current Workspace Directory ({workspace}) Files\na.rs\n\n# Current Mode\n<slug>code</slug>\n<model>{model}</model>\n</environment_details>"
        )})
    }

    fn text(t: &str) -> serde_json::Value {
        json!({ "type": "text", "text": t })
    }

    fn tool_calls(session: &Session) -> Vec<(String, Option<String>, serde_json::Value)> {
        session
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match p {
                Part::ToolCall { name, id, input } => Some((name.clone(), id.clone(), input.clone().unwrap())),
                _ => None,
            })
            .collect()
    }

    fn results(session: &Session) -> Vec<(Option<String>, String, Option<bool>)> {
        session
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match p {
                Part::ToolResult { id, output, is_error, .. } => Some((id.clone(), output.clone().unwrap(), *is_error)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn reads_an_xml_protocol_roo_task() {
        let storage = Storage::new("xml");
        storage.task("parent-task", &[]);
        let history = json!([
            { "role": "user", "ts": 1000, "content": [text("<task>\nFix the build\nplease\n</task>"), env("/Users/me/app", "claude-sonnet-4-5:thinking")] },
            { "role": "assistant", "ts": 1001, "content": [text(
                "<thinking>\nLook first.\n</thinking>\n\nReading it.\n\n<read_file>\n<args>\n  <file>\n    <path>src/a.rs</path>\n  </file>\n</args>\n</read_file>\n\n[Response interrupted by a tool use result. Only one tool may be used at a time and should be placed at the end of the message.]"
            )] },
            { "role": "user", "ts": 1002, "content": [
                text("[read_file for 'src/a.rs'. Reading multiple files at once is more efficient.] Result:"),
                text("<files><file><path>src/a.rs</path><content>fn a() {}</content></file></files>"),
                text("Tool [apply_diff] was not executed because a tool has already been used in this message."),
                env("/Users/me/app", "claude-opus-4-6"),
            ] },
            { "role": "assistant", "ts": 1003, "content": [text(
                "<apply_diff>\n<args>\n<file>\n<path>src/a.rs</path>\n<diff>\n<content>\n```\n<<<<<<< SEARCH\nfn a() {}\n=======\nfn a() { b() }\n>>>>>>> REPLACE\n```\n</content>\n<start_line>3</start_line>\n</diff>\n</file>\n</args>\n</read_file>"
            )] },
            { "role": "user", "ts": 1004, "content": [text("[apply_diff for 'src/a.rs'] Result:"), text("Changes applied.")] },
            { "role": "assistant", "ts": 1005, "isSummary": true, "content": [text("## Summary\nFixed a.")] },
            { "role": "assistant", "ts": 1006, "content": [text("<update_todo_list>\n<todos>\n[x] read\n[-] fix\n</todos>\n</update_todo_list>")] },
            { "role": "user", "ts": 1007, "content": [text("[update_todo_list] Result:"), text("Todo list updated successfully.")] },
            { "role": "assistant", "ts": 1008, "content": [text("<attempt_completion>\n<result>\nDone.\n</result>\n</attempt_completion>")] },
            { "role": "user", "ts": 1009, "content": [
                text("[attempt_completion] Result:"),
                text("The user has provided feedback on the results.\n<feedback>\nAlso run tests\n</feedback>"),
                { "type": "image", "source": { "type": "base64", "media_type": "image/webp", "data": "UklG" } },
            ] },
        ]);
        let dir = storage.task("t-xml", &[
            ("api_conversation_history.json", history),
            ("history_item.json", json!({ "id": "t-xml", "ts": 2000, "task": "Fix the build\nplease", "parentTaskId": "parent-task" })),
        ]);
        let session = Cline::read(&dir).unwrap();

        assert_eq!(session.source_id, "t-xml");
        assert_eq!(session.title, "Fix the build");
        assert_eq!(session.directory.as_deref(), Some("/Users/me/app"), "from the environment details");
        assert_eq!(session.parent.as_deref(), Some("parent-task"));
        assert_eq!((session.time_created, session.time_updated), (1000, 2000));
        assert_eq!(session.messages[0].parts[0].as_text(), Some("Fix the build\nplease"));
        assert_eq!(session.messages[0].parts.len(), 1, "environment details dropped");

        let first = &session.messages[1];
        assert_eq!(first.model.as_ref().map(|m| (m.provider.as_str(), m.id.as_str())), Some(("anthropic", "claude-sonnet-4-5")));
        assert!(matches!(&first.parts[0], Part::Reasoning { text, .. } if text == "Look first."));
        assert_eq!(first.parts[1].as_text(), Some("Reading it."));
        assert_eq!(first.parts.len(), 3, "the interruption notice is dropped");
        assert_eq!(session.messages[3].model.as_ref().unwrap().id, "claude-opus-4-6");

        let calls = tool_calls(&session);
        assert_eq!(calls[0].0, "read_file");
        assert_eq!(calls[0].2, json!({ "args": { "file": { "path": "src/a.rs" } } }));
        assert_eq!(calls[1].0, "apply_diff", "a call closed with the wrong tag still counts");
        assert_eq!(calls[1].2["args"]["file"]["diff"]["start_line"], "3");
        assert_eq!(calls[2].0, "update_todo_list");
        assert_eq!(calls.len(), 3, "attempt_completion becomes text");

        let results = results(&session);
        assert_eq!(results[0].0, calls[0].1);
        assert!(results[0].1.contains("fn a() {}"));
        assert_eq!(results[1].0, calls[1].1);
        assert_eq!(results[1].1, "Changes applied.");
        assert_eq!(results.len(), 3, "the unpaired `not executed` notice stays text");

        assert!(session.messages[5].summary);
        let last = session.messages.last().unwrap();
        assert_eq!(last.parts[0].as_text(), Some("Also run tests"));
        assert!(matches!(&last.parts[1], Part::Attachment { mime, .. } if mime == "image/webp"));
        assert_eq!(session.messages[session.messages.len() - 2].parts[0].as_text(), Some("Done."));
    }

    #[test]
    fn reads_a_native_protocol_task() {
        let storage = Storage::new("native");
        let history = json!([
            { "role": "user", "ts": 10, "content": [text("<user_message>\nlook\n</user_message>")] },
            { "role": "assistant", "ts": 11, "content": [
                { "type": "reasoning", "text": "hmm", "summary": [] },
                { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                { "type": "tool_use", "id": "toolu_1", "name": "mcp--dh___serper___mcp--google___search", "input": { "q": "x" } },
                { "type": "tool_use", "id": "toolu_2", "name": "new_task", "input": { "mode": "code", "message": "sub" } },
            ] },
            { "role": "user", "ts": 12, "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    { "type": "text", "text": "found" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBO" } },
                ] },
                { "type": "tool_result", "tool_use_id": "toolu_2", "content": "spawned" },
            ] },
        ]);
        let dir = storage.task("t-native", &[
            ("api_conversation_history.json", history),
            ("history_item.json", json!({ "id": "t-native", "ts": 12, "task": "<task>\nlook\n</task>\n<extra>", "workspace": "/w" })),
        ]);
        storage.task("child-b", &[("history_item.json", json!({ "ts": 30, "parentTaskId": "t-native" }))]);
        storage.task("child-a", &[("history_item.json", json!({ "ts": 20, "parentTaskId": "t-native" }))]);
        let session = Cline::read(&dir.join(HISTORY_FILE)).unwrap();

        assert_eq!(session.directory.as_deref(), Some("/w"));
        assert_eq!(session.title, "look", "tag-only lines are not a title");
        assert_eq!(session.parent, None);
        assert_eq!(session.children, ["child-a", "child-b"]);
        assert_eq!(session.messages[0].parts[0].as_text(), Some("look"));
        let reasoning: Vec<_> = session.messages[1]
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Reasoning { signature, .. } => Some(signature.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, [Some("sig".to_string())], "Kilo's unsigned copy of a signed thought is dropped");
        let results = results(&session);
        assert_eq!(results[0], (Some("toolu_1".into()), "found".into(), None));
        assert!(matches!(&session.messages[2].parts[1], Part::Attachment { mime, .. } if mime == "image/png"));
    }

    #[test]
    fn reads_cline_times_models_and_workspace_from_its_side_files() {
        let storage = Storage::new("cline");
        let history = json!([
            { "role": "user", "content": [text("<task>\nhi\n</task>")] },
            { "role": "assistant", "content": [text("<execute_command>\n<command>ls</command>\n<requires_approval>false</requires_approval>\n</execute_command>")] },
            { "role": "user", "content": [text("[execute_command for 'ls'] Result:\nCommand executed.\nOutput:\na.rs")] },
            { "role": "assistant", "content": [text("<plan_mode_response>\n<response>Plan.</response>\n</plan_mode_response>")] },
        ]);
        let ui = json!([
            { "ts": 1_000, "type": "say", "say": "text", "conversationHistoryIndex": -1 },
            { "ts": 1_500, "type": "say", "say": "text", "conversationHistoryIndex": 0 },
            { "ts": 2_000, "type": "ask", "ask": "command_output", "conversationHistoryIndex": 1 },
        ]);
        let usage = json!({ "model_usage": [
            { "ts": 1_400, "model_id": "claude-3-7-sonnet-20250219", "model_provider_id": "anthropic" },
        ]});
        let dir = storage.task("1000", &[
            ("api_conversation_history.json", history),
            ("ui_messages.json", ui),
            ("task_metadata.json", usage),
        ]);
        std::fs::create_dir_all(storage.0.join("state")).unwrap();
        std::fs::write(
            storage.0.join("state").join("taskHistory.json"),
            json!([{ "id": "1000", "ts": 3_000, "task": "hi", "cwdOnTaskInitialization": "/c" }]).to_string(),
        )
        .unwrap();
        let session = Cline::read(&dir).unwrap();

        let times: Vec<i64> = session.messages.iter().map(|m| m.time_created).collect();
        assert_eq!(times, [1_000, 1_500, 2_000, 2_000], "an api message without ui messages keeps the last time");
        assert_eq!(session.directory.as_deref(), Some("/c"));
        assert_eq!(session.messages[1].model.as_ref().unwrap().id, "claude-3-7-sonnet-20250219");
        let calls = tool_calls(&session);
        assert_eq!(calls[0].2, json!({ "command": "ls", "requires_approval": "false" }));
        assert_eq!(results(&session)[0].1, "Command executed.\nOutput:\na.rs");
        assert_eq!(session.messages[3].parts[0].as_text(), Some("Plan."));
    }
}
