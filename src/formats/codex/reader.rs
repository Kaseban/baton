//! A thread's records as canonical messages.
//!
//! What the model said and called comes from `response_item`s; what a
//! code-mode script did comes from the `item_completed` events it produced,
//! each turned into the call it stands for (a command, a file change, an MCP
//! call). A script whose every effect such an item (or a parsed `update_plan`)
//! represents is dropped in favour of them; any other script is kept as the
//! `exec` call it was.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::items::{self, Output};
use super::rollout::Record;
use super::script;
use crate::canonical::{Agent, Message, ModelRef, Part, Role};

/// What stands in for content Codex encrypted: the task and messages agents
/// send each other, readable only by OpenAI.
const ENCRYPTED: &str = "[encrypted by Codex]";

/// How much of the assistant's own words a compaction summary carries over.
const SUMMARY_TAIL_CHARS: usize = 20_000;

struct Script {
    call_id: String,
    tools: Vec<String>,
    items: usize,
    plans: usize,
}

impl Script {
    /// Whether the items and plans read from this script stand for all it did.
    fn represented(&self) -> bool {
        let covered = |t: &String| {
            matches!(t.as_str(), "exec_command" | "apply_patch" | "update_plan" | "web__run") || t.starts_with("mcp__")
        };
        !self.tools.is_empty()
            && self.tools.iter().all(covered)
            && (self.items > 0 || (self.plans > 0 && self.tools.iter().all(|t| t == "update_plan")))
    }
}

pub(super) struct Reader {
    provider: String,
    model: Option<ModelRef>,
    messages: Vec<Message>,
    current: Option<Message>,
    /// A tool output closed the current step; the model's next output starts
    /// a new assistant message, as opencode records one per step.
    after_output: bool,
    scripts: Vec<Script>,
    /// Direct (non code-mode) command and patch calls still awaiting output;
    /// they report their own results, so items meanwhile would duplicate them.
    direct: HashSet<String>,
    agent_threads: HashMap<String, String>,
    /// `spawn_agent` call ids, with the agent path each one started.
    spawns: Vec<(String, Option<String>)>,
    recent_texts: Vec<String>,
    last_ts: i64,
}

impl Reader {
    pub fn new(provider: String, start: i64) -> Self {
        Reader {
            last_ts: start,
            provider,
            model: None,
            messages: Vec::new(),
            current: None,
            after_output: false,
            scripts: Vec::new(),
            direct: HashSet::new(),
            agent_threads: HashMap::new(),
            spawns: Vec::new(),
            recent_texts: Vec::new(),
        }
    }

    /// The messages, and the thread ids of the subagents each `spawn_agent`
    /// call started, in call order (empty where one could not be resolved).
    pub fn finish(mut self) -> (Vec<Message>, Vec<String>) {
        self.flush();
        let children = self
            .spawns
            .iter()
            .map(|(_, path)| path.as_ref().and_then(|p| self.agent_threads.get(p)).cloned().unwrap_or_default())
            .collect();
        self.messages.retain(|m| !m.parts.is_empty());
        (self.messages, children)
    }

    pub fn record(&mut self, r: &Record) {
        let p = &r.payload;
        // Lines without a timestamp (legacy rollouts) happened after the last one with one.
        if r.ts > 0 {
            self.last_ts = r.ts;
        }
        let ts = self.last_ts;
        match r.kind.as_str() {
            "turn_context" => {
                if let Some(model) = p["model"].as_str() {
                    self.model = Some(ModelRef { provider: self.provider.clone(), id: model.into() });
                }
            }
            "event_msg" if p["type"] == "item_completed" => self.item(&p["item"], ts),
            "compacted" => self.compacted(p, ts),
            "response_item" => self.response_item(p, ts),
            _ => {}
        }
    }

    fn ensure(&mut self, ts: i64) -> &mut Message {
        self.current.get_or_insert_with(|| Message {
            role: Role::Assistant,
            parts: Vec::new(),
            time_created: ts,
            origin: Some(Agent::Codex),
            model: self.model.clone(),
            summary: false,
        })
    }

    fn flush(&mut self) {
        if let Some(msg) = self.current.take() {
            self.messages.push(msg);
        }
        self.after_output = false;
    }

    fn model_parts(&mut self, parts: Vec<Part>, ts: i64) {
        if self.after_output {
            self.flush();
        }
        self.ensure(ts).parts.extend(parts);
    }

    fn output_parts(&mut self, parts: Vec<Part>, ts: i64) {
        self.ensure(ts).parts.extend(parts);
        self.after_output = true;
    }

    fn user(&mut self, parts: Vec<Part>, ts: i64) {
        self.flush();
        self.messages.push(Message {
            role: Role::User,
            parts,
            time_created: ts,
            origin: Some(Agent::Codex),
            model: None,
            summary: false,
        });
    }

    fn item(&mut self, item: &Value, ts: i64) {
        if item["type"] == "SubAgentActivity" && item["kind"] == "started" {
            if let (Some(path), Some(id)) = (item["agent_path"].as_str(), item["agent_thread_id"].as_str()) {
                self.agent_threads.insert(path.into(), id.into());
            }
            return;
        }
        let Some(parts) = items::item(item) else { return };
        if self.scripts.is_empty() && !self.direct.is_empty() {
            return;
        }
        if let Some(script) = self.scripts.last_mut() {
            script.items += 1;
        }
        self.output_parts(parts, ts);
    }

    fn response_item(&mut self, p: &Value, ts: i64) {
        let call_id = p["call_id"].as_str().or(p["id"].as_str()).unwrap_or("call").to_string();
        match p["type"].as_str() {
            Some("message") => match p["role"].as_str() {
                Some("user") => {
                    let parts = user_content(&p["content"]);
                    if !parts.is_empty() {
                        self.user(parts, ts);
                    }
                }
                Some("assistant") => {
                    let texts: Vec<String> = text_items(&p["content"]).into_iter().filter(|t| !t.is_empty()).collect();
                    self.recent_texts.extend(texts.iter().cloned());
                    self.model_parts(texts.into_iter().map(|text| Part::Text { text }).collect(), ts);
                }
                // developer / system: the instructions Codex injects.
                _ => {}
            },
            Some("reasoning") => {
                let blocks = p["summary"].as_array().into_iter().chain(p["content"].as_array()).flatten();
                let text: Vec<&str> = blocks.filter_map(|b| b["text"].as_str()).filter(|t| !t.is_empty()).collect();
                if p["text"].as_str().is_some_and(|t| !t.is_empty()) {
                    self.model_parts(vec![Part::Reasoning { text: p["text"].as_str().unwrap_or("").into(), signature: None }], ts);
                } else if !text.is_empty() {
                    self.model_parts(vec![Part::Reasoning { text: text.join("\n\n"), signature: None }], ts);
                }
            }
            Some(kind @ ("function_call" | "local_shell_call")) => {
                let (name, args) = if kind == "local_shell_call" {
                    let action = &p["action"];
                    ("local_shell", json!({ "command": action["command"], "workdir": action["working_directory"] }))
                } else {
                    let raw = p["arguments"].as_str().unwrap_or("{}");
                    (p["name"].as_str().unwrap_or("tool"), serde_json::from_str(raw).unwrap_or_else(|_| json!(raw)))
                };
                let (name, mut input) = items::function_call(name, args);
                if input["message"].as_str().is_some_and(encrypted) {
                    input["message"] = json!(ENCRYPTED);
                }
                if matches!(name.as_str(), "exec_command" | "apply_patch") {
                    self.direct.insert(call_id.clone());
                }
                if name == "spawn_agent" {
                    self.spawns.push((call_id.clone(), None));
                }
                self.model_parts(vec![Part::ToolCall { name, id: Some(call_id), input: Some(input) }], ts);
            }
            Some("custom_tool_call") => self.custom_call(p, call_id, ts),
            Some("function_call_output" | "custom_tool_call_output" | "local_shell_call_output") => {
                self.call_output(p, call_id, ts)
            }
            Some("agent_message") => self.agent_message(p, ts),
            Some("web_search_call") => {
                let action = &p["action"];
                let query = action["query"].as_str().map(str::to_string).unwrap_or_else(|| {
                    let qs = action["queries"].as_array().into_iter().flatten().filter_map(Value::as_str);
                    qs.collect::<Vec<_>>().join(" | ")
                });
                self.model_parts(Vec::new(), ts);
                let parts = items::call_and_result("web_search", &call_id, json!({ "query": query }), Output::text(""));
                self.output_parts(parts, ts);
            }
            _ => {}
        }
    }

    fn custom_call(&mut self, p: &Value, call_id: String, ts: i64) {
        let input = p["input"].as_str().unwrap_or_default();
        match p["name"].as_str().unwrap_or("tool") {
            "exec" => {
                let call = Part::ToolCall { name: "exec".into(), id: Some(call_id.clone()), input: Some(json!({ "code": input })) };
                self.model_parts(vec![call], ts);
                let plans = script::literal_args(input, "update_plan");
                let script = Script { call_id: call_id.clone(), tools: script::tools(input), items: 0, plans: plans.len() };
                for (n, plan) in plans.into_iter().enumerate() {
                    let id = format!("{call_id}-plan{n}");
                    let parts = items::call_and_result("update_plan", &id, plan, Output::text("Plan updated"));
                    self.ensure(ts).parts.extend(parts);
                }
                self.scripts.push(script);
            }
            "apply_patch" => {
                self.direct.insert(call_id.clone());
                let call = Part::ToolCall { name: "apply_patch".into(), id: Some(call_id), input: Some(json!({ "patchText": input })) };
                self.model_parts(vec![call], ts);
            }
            name => {
                let input = serde_json::from_str::<Value>(input).ok().filter(Value::is_object).unwrap_or_else(|| json!({ "input": input }));
                self.model_parts(vec![Part::ToolCall { name: name.into(), id: Some(call_id), input: Some(input) }], ts);
            }
        }
    }

    fn call_output(&mut self, p: &Value, call_id: String, ts: i64) {
        self.direct.remove(&call_id);
        if let Some(at) = self.scripts.iter().position(|s| s.call_id == call_id) {
            let script = self.scripts.remove(at);
            if script.represented() {
                self.remove_call(&call_id);
                self.after_output = true;
                return;
            }
        }
        if let Some(spawn) = self.spawns.iter_mut().find(|(id, _)| *id == call_id) {
            let out: Option<Value> = p["output"].as_str().and_then(|o| serde_json::from_str(o).ok());
            spawn.1 = out.and_then(|o| o["task_name"].as_str().map(str::to_string));
        }
        self.output_parts(items::result("tool", &call_id, items::output(&p["output"])), ts);
    }

    /// Drop the call `id` from the step it was made in.
    fn remove_call(&mut self, id: &str) {
        let is_call = |part: &Part| matches!(part, Part::ToolCall { id: Some(c), .. } if c == id);
        for msg in self.current.iter_mut().chain(self.messages.iter_mut().rev().take(16)) {
            if let Some(at) = msg.parts.iter().position(is_call) {
                msg.parts.remove(at);
                return;
            }
        }
    }

    /// A message from another agent: a subagent's task or a message from its
    /// parent (encrypted), or a child's report.
    fn agent_message(&mut self, p: &Value, ts: i64) {
        let mut text: String = text_items(&p["content"]).concat();
        if p["content"].as_array().into_iter().flatten().any(|c| c["type"] == "encrypted_content") {
            text.push_str(ENCRYPTED);
        }
        if !text.is_empty() {
            self.user(vec![Part::Text { text }], ts);
        }
    }

    /// Codex's compaction kept the user's requests and an encrypted summary;
    /// the summary stands in for everything before it with what is readable.
    fn compacted(&mut self, p: &Value, ts: i64) {
        self.flush();
        let kept: Vec<String> = p["replacement_history"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["type"] == "message" && m["role"] == "user")
            .flat_map(|m| user_content(&m["content"]))
            .filter_map(|part| part.as_text().map(str::to_string))
            .collect();
        let mut summary = String::from(
            "Codex compacted the conversation here. Its own summary of what came before is encrypted, so it could not be carried over.",
        );
        if let Some(message) = p["message"].as_str().filter(|m| !m.trim().is_empty()) {
            summary = message.to_string();
        }
        if !kept.is_empty() {
            summary.push_str("\n\nThe user's requests it kept:\n");
            for request in &kept {
                summary.push_str(&format!("\n- {}", request.trim().replace('\n', "\n  ")));
            }
        }
        let mut tail: Vec<&str> = Vec::new();
        let mut size = 0;
        for text in self.recent_texts.iter().rev() {
            if size + text.len() > SUMMARY_TAIL_CHARS && !tail.is_empty() {
                break;
            }
            size += text.len();
            tail.push(text);
        }
        if !tail.is_empty() {
            tail.reverse();
            summary.push_str("\n\nWhat the assistant said last before the compaction:\n\n");
            summary.push_str(&tail.join("\n\n---\n\n"));
        }
        self.recent_texts.clear();
        self.messages.push(Message {
            role: Role::Assistant,
            parts: vec![Part::Text { text: summary }],
            time_created: ts,
            origin: Some(Agent::Codex),
            model: self.model.clone(),
            summary: true,
        });
    }
}

/// An encrypted token (Fernet) in place of readable text.
fn encrypted(text: &str) -> bool {
    text.starts_with("gAAAAA") && !text.contains(char::is_whitespace)
}

fn text_items(content: &Value) -> Vec<String> {
    let texts = content.as_array().into_iter().flatten();
    texts.filter_map(|c| c["text"].as_str().map(str::to_string)).collect()
}

/// A user message's own words and images, without the context Codex injects
/// as user text (`# AGENTS.md instructions`, `<environment_context>`, ...).
pub(super) fn user_content(content: &Value) -> Vec<Part> {
    let mut parts = Vec::new();
    for c in content.as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("input_text" | "text") => {
                let text = c["text"].as_str().unwrap_or_default();
                if !text.trim().is_empty() && !injected(text) {
                    parts.push(Part::Text { text: text.to_string() });
                }
            }
            Some("input_image") => parts.extend(c["image_url"].as_str().and_then(items::image)),
            Some("local_image") => parts.extend(c["path"].as_str().and_then(items::image)),
            _ => {}
        }
    }
    parts
}

fn injected(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with("# AGENTS.md instructions") {
        return true;
    }
    let Some(tag) = t.strip_prefix('<').and_then(|rest| rest.split('>').next()) else {
        return false;
    };
    !tag.is_empty()
        && tag != "user_shell_command"
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '))
        && t.ends_with(&format!("</{tag}>"))
}
