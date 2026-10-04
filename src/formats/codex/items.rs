//! Codex tool calls, and the items recording what they did, as canonical
//! parts. Calls are named in Codex's terms (`exec_command`, `apply_patch`,
//! `update_plan`, `web_search`, `mcp__<server>__<tool>`); the opencode writer
//! maps them onto its own tools.

use serde_json::{Map, Value, json};

use crate::canonical::Part;

/// What a tool returned.
pub(super) struct Output {
    pub text: String,
    pub is_error: Option<bool>,
    pub images: Vec<Part>,
}

impl Output {
    pub fn text(text: impl Into<String>) -> Self {
        Output { text: text.into(), is_error: None, images: Vec::new() }
    }
}

/// A call and its result, as the parts the reader appends.
pub(super) fn call_and_result(name: &str, id: &str, input: Value, out: Output) -> Vec<Part> {
    let mut parts = vec![Part::ToolCall { name: name.into(), id: Some(id.into()), input: Some(input) }];
    parts.extend(result(name, id, out));
    parts
}

pub(super) fn result(name: &str, id: &str, out: Output) -> Vec<Part> {
    let mut parts = vec![Part::ToolResult {
        name: name.into(),
        id: Some(id.into()),
        output: Some(out.text),
        is_error: out.is_error,
    }];
    parts.extend(out.images);
    parts
}

/// The call an `item_completed` item stands for, with its result: a command,
/// a file change, an MCP tool call or a web search. Other items mirror
/// records the reader takes from elsewhere.
pub(super) fn item(item: &Value) -> Option<Vec<Part>> {
    let id = item["id"].as_str().unwrap_or("item");
    let s = |k: &str| item[k].as_str().filter(|v| !v.is_empty());
    let failed = item["status"] == "failed";
    match item["type"].as_str()? {
        "CommandExecution" => {
            let mut input = json!({ "command": shell_command(&item["command"]) });
            if let Some(cwd) = s("cwd") {
                input["workdir"] = json!(file_url_path(cwd));
            }
            let text = s("aggregated_output")
                .or_else(|| s("formatted_output"))
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}{}", s("stdout").unwrap_or(""), s("stderr").unwrap_or("")));
            let exit = item["exit_code"].as_i64();
            let mut out = Output::text(exec_output(exit, &text));
            out.is_error = (exit.is_none() && failed).then_some(true);
            Some(call_and_result("exec_command", id, input, out))
        }
        "FileChange" => {
            let changes = item["changes"].as_object()?;
            let summary: Vec<String> = changes
                .iter()
                .map(|(path, c)| match c["type"].as_str() {
                    Some("add") => format!("A {path}"),
                    Some("delete") => format!("D {path}"),
                    _ => format!("M {}", c["move_path"].as_str().unwrap_or(path)),
                })
                .collect();
            let text = s("stdout")
                .map(str::to_string)
                .unwrap_or_else(|| format!("Success. Updated the following files:\n{}\n", summary.join("\n")));
            let mut out = Output::text(text);
            out.is_error = (item["status"] != "completed").then_some(true);
            Some(call_and_result("apply_patch", id, json!({ "patchText": patch_text(changes) }), out))
        }
        "McpToolCall" => {
            let name = format!("mcp__{}__{}", s("server")?, s("tool")?);
            let input = match &item["arguments"] {
                Value::String(raw) => serde_json::from_str(raw).unwrap_or_else(|_| json!({ "arguments": raw })),
                Value::Null => json!({}),
                args => args.clone(),
            };
            let mut out = content_output(&item["result"]["content"]);
            if let Some(err) = item["error"].as_str().or(item["error"]["message"].as_str()) {
                out.text = if out.text.is_empty() { err.to_string() } else { format!("{err}\n{}", out.text) };
            }
            out.is_error = failed.then_some(true);
            Some(call_and_result(&name, id, input, out))
        }
        "Extension" if item["kind"] == "web.search" => {
            let queries: Vec<&str> = match item["action"]["queries"].as_array() {
                Some(qs) => qs.iter().filter_map(Value::as_str).collect(),
                None => s("query").into_iter().collect(),
            };
            let results: Vec<String> = item["results"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    ["title", "url", "snippet"]
                        .iter()
                        .filter_map(|k| r[*k].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect();
            let input = json!({ "query": queries.join(" | ") });
            Some(call_and_result("web_search", id, input, Output::text(results.join("\n\n"))))
        }
        _ => None,
    }
}

/// A call the model made directly, renamed and reshaped to the canonical form
/// where Codex has had several names for the same tool.
pub(super) fn function_call(name: &str, args: Value) -> (String, Value) {
    let workdir = |input: &mut Value| {
        if let Some(dir) = args["workdir"].as_str().or(args["working_directory"].as_str()) {
            input["workdir"] = json!(dir);
        }
    };
    match name {
        "shell" | "container.exec" | "local_shell" => {
            let mut input = json!({ "command": shell_command(&args["command"]) });
            workdir(&mut input);
            ("exec_command".into(), input)
        }
        "exec_command" => {
            let command = args["cmd"].as_str().or(args["command"].as_str()).unwrap_or_default();
            let mut input = json!({ "command": command });
            workdir(&mut input);
            ("exec_command".into(), input)
        }
        "apply_patch" => {
            let patch = args.as_str().or(args["input"].as_str()).or(args["patch"].as_str()).unwrap_or_default();
            ("apply_patch".into(), json!({ "patchText": patch }))
        }
        _ => (name.into(), args),
    }
}

/// A tool output in any of the shapes Codex has recorded one in.
pub(super) fn output(v: &Value) -> Output {
    match v {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            // Old `shell`: {"output": "...", "metadata": {"exit_code": 0}}.
            Ok(o) if o["output"].is_string() => {
                Output::text(exec_output(o["metadata"]["exit_code"].as_i64(), o["output"].as_str().unwrap_or("")))
            }
            _ => Output::text(s.clone()),
        },
        Value::Array(_) => content_output(v),
        Value::Object(o) => Output {
            text: o.get("content").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| v.to_string()),
            is_error: o.get("success").and_then(Value::as_bool).map(|ok| !ok),
            images: Vec::new(),
        },
        _ => Output::text(""),
    }
}

/// Text and images of a content list (`input_text`, `input_image`, MCP `text`
/// and `image` blocks).
fn content_output(content: &Value) -> Output {
    let mut out = Output::text("");
    for c in content.as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("input_text" | "output_text" | "text") => out.text.push_str(c["text"].as_str().unwrap_or("")),
            Some("input_image") => out.images.extend(c["image_url"].as_str().and_then(image)),
            Some("image") => out.images.extend(c["data"].as_str().map(|data| Part::Attachment {
                mime: c["mimeType"].as_str().unwrap_or("image/png").into(),
                path: None,
                data: Some(data.into()),
            })),
            _ => {}
        }
    }
    out
}

/// An image as Codex records one: a data URL, or a path.
pub(super) fn image(url: &str) -> Option<Part> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (mime, data) = rest.split_once(";base64,")?;
        return Some(Part::Attachment { mime: mime.into(), path: None, data: Some(data.into()) });
    }
    let path = file_url_path(url);
    let ext = path.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };
    path.starts_with('/').then(|| Part::Attachment { mime: mime.into(), path: Some(path), data: None })
}

/// How Codex reports a finished command to the model.
fn exec_output(exit: Option<i64>, text: &str) -> String {
    match exit {
        Some(code) => format!("Process exited with code {code}\nOutput:\n{text}"),
        None => text.to_string(),
    }
}

/// A command line from Codex's argv, unwrapping `zsh -lc '<script>'`.
fn shell_command(command: &Value) -> String {
    let argv: Vec<&str> = match command {
        Value::String(s) => return s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return String::new(),
    };
    match argv.as_slice() {
        [shell, flag, script] if shell.ends_with("sh") && matches!(*flag, "-lc" | "-c") => script.to_string(),
        _ => argv
            .iter()
            .map(|a| {
                if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
                    a.to_string()
                } else {
                    format!("'{}'", a.replace('\'', "'\\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// `file:///a%20b` as `/a b`; anything else unchanged.
fn file_url_path(url: &str) -> String {
    let Some(path) = url.strip_prefix("file://") else { return url.to_string() };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file change in Codex's own patch format, with every added file's full
/// content and every update's hunks.
fn patch_text(changes: &Map<String, Value>) -> String {
    let mut out = String::from("*** Begin Patch\n");
    for (path, c) in changes {
        match c["type"].as_str() {
            Some("add") => {
                out.push_str(&format!("*** Add File: {path}\n"));
                for line in c["content"].as_str().unwrap_or("").lines() {
                    out.push_str(&format!("+{line}\n"));
                }
            }
            Some("delete") => out.push_str(&format!("*** Delete File: {path}\n")),
            _ => {
                out.push_str(&format!("*** Update File: {path}\n"));
                if let Some(to) = c["move_path"].as_str() {
                    out.push_str(&format!("*** Move to: {to}\n"));
                }
                let diff = c["unified_diff"].as_str().unwrap_or("");
                let hunks = diff.find("@@").map_or("", |at| &diff[at..]);
                for line in hunks.lines() {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str("*** End Patch");
    out
}
