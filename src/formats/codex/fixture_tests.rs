//! A scratch Codex home with a Desktop thread spanning two windows, a
//! subagent it spawned and a fork of it, read back and written to opencode.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Codex, info};
use crate::canonical::{Format, Part, Role, Session};
use crate::formats::opencode::Opencode;

const PARENT: &str = "01a00000-0000-7000-8000-000000000001";
const WINDOW: &str = "01a00000-0000-7000-8000-0000000000aa";
const CHILD: &str = "01a00000-0000-7000-8000-000000000002";
const FORK: &str = "01a00000-0000-7000-8000-000000000003";

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn line(ordinal: i64, kind: &str, payload: Value) -> Value {
    json!({ "timestamp": format!("2026-09-01T10:00:{:02}.000Z", ordinal % 60), "ordinal": ordinal, "type": kind, "payload": payload })
}

fn write(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(path, body.join("\n") + "\n").unwrap();
}

fn item(ordinal: i64, item: Value) -> Value {
    line(ordinal, "event_msg", json!({ "type": "item_completed", "item": item }))
}

fn message(ordinal: i64, role: &str, kind: &str, texts: &[&str]) -> Value {
    let content: Vec<Value> = texts.iter().map(|t| json!({ "type": kind, "text": t })).collect();
    line(ordinal, "response_item", json!({ "type": "message", "role": role, "content": content }))
}

fn parent_meta() -> Value {
    json!({ "id": PARENT, "cwd": "/Volumes/projects/app", "originator": "Codex Desktop", "source": "vscode",
            "thread_source": "user", "model_provider": "openai", "timestamp": "2026-09-01T10:00:00.000Z" })
}

fn home() -> Home {
    let root = std::env::temp_dir().join(format!("baton-codex-home-{}", std::process::id()));
    let day = root.join("sessions/2026/09/01");
    let script = r#"const r = await tools.exec_command({cmd:"ls"}); await tools.apply_patch("*** Begin Patch"); await tools.update_plan({plan:[{step:"Fix it",status:"in_progress"}]}); text(r.output);"#;
    write(&day.join(format!("rollout-2026-09-01T10-00-00-{PARENT}.jsonl")), &[
        line(0, "session_meta", parent_meta()),
        message(1, "developer", "input_text", &["<permissions instructions>sandbox</permissions instructions>"]),
        message(2, "user", "input_text", &["# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\nbe nice\n</INSTRUCTIONS>", "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>"]),
        line(3, "turn_context", json!({ "model": "gpt-5.6-sol" })),
        line(4, "response_item", json!({ "type": "message", "role": "user", "content": [
            { "type": "input_text", "text": "fix the bug" },
            { "type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo=" },
        ]})),
        line(5, "response_item", json!({ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "**Planning**" }], "encrypted_content": "gAAAAAopaque" })),
        line(6, "response_item", json!({ "type": "custom_tool_call", "name": "exec", "call_id": "call_1", "input": script })),
        item(7, json!({ "type": "CommandExecution", "id": "exec-1", "command": ["/bin/zsh", "-lc", "ls"], "cwd": "file:///Volumes/projects/app",
                        "aggregated_output": "a.txt\n", "exit_code": 2, "status": "failed" })),
        item(8, json!({ "type": "FileChange", "id": "exec-2", "status": "completed", "changes": {
            "/Volumes/projects/app/new.txt": { "type": "add", "content": "hello\nworld\n" },
            "/Volumes/projects/app/a.txt": { "type": "update", "unified_diff": "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-old\n+new\n", "move_path": null },
        }})),
        line(9, "response_item", json!({ "type": "custom_tool_call_output", "call_id": "call_1", "output": [{ "type": "input_text", "text": "Script completed" }] })),
        line(10, "response_item", json!({ "type": "function_call", "name": "spawn_agent", "call_id": "call_2",
            "arguments": json!({ "message": "gAAAAAsecret", "task_name": "child", "agent_type": "explorer" }).to_string() })),
        item(11, json!({ "type": "SubAgentActivity", "id": "sa", "kind": "started", "agent_thread_id": CHILD, "agent_path": "/root/child" })),
        line(12, "response_item", json!({ "type": "function_call_output", "call_id": "call_2", "output": "{\"task_name\":\"/root/child\"}" })),
        message(13, "assistant", "output_text", &["done"]),
        line(14, "compacted", json!({ "message": "", "replacement_history": [
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "fix the bug" }] },
            { "type": "compaction", "encrypted_content": "gAAAAAsummary" },
        ]})),
        line(15, "turn_context", json!({ "model": "gpt-5.6-luna" })),
        message(16, "user", "input_text", &["and now?"]),
    ]);
    write(&day.join(format!("rollout-2026-09-01T10-05-00-{PARENT}_{WINDOW}.jsonl")), &[
        line(0, "session_meta", parent_meta()),
        message(1, "assistant", "output_text", &["later"]),
    ]);
    write(&day.join(format!("rollout-2026-09-01T10-01-00-{CHILD}.jsonl")), &[
        line(0, "session_meta", json!({ "id": CHILD, "cwd": "/Volumes/projects/app", "originator": "Codex Desktop",
            "source": { "subagent": { "thread_spawn": { "parent_thread_id": PARENT, "agent_path": "/root/child", "agent_nickname": "Explorer" } } },
            "thread_source": "subagent", "subagent_history_start_ordinal": 3 })),
        line(1, "session_meta", parent_meta()),
        message(2, "user", "input_text", &["fix the bug"]),
        line(3, "response_item", json!({ "type": "agent_message", "author": "/root", "recipient": "/root/child", "content": [
            { "type": "input_text", "text": "Message Type: NEW_TASK\nPayload:\n" }, { "type": "encrypted_content", "encrypted_content": "gAAAAA" },
        ]})),
        message(4, "assistant", "output_text", &["child answer"]),
    ]);
    write(&root.join(format!("archived_sessions/rollout-2026-09-01T11-00-00-{FORK}.jsonl")), &[
        line(5, "session_meta", json!({ "id": FORK, "cwd": "/Volumes/projects/app", "source": "vscode", "forked_from_id": PARENT,
            "history_base": { "thread_id": PARENT, "end_ordinal_exclusive": 5 } })),
        message(6, "user", "input_text", &["fork prompt"]),
    ]);
    std::fs::write(
        root.join("session_index.jsonl"),
        format!("{}\n{}\n", json!({ "id": PARENT, "thread_name": "Old name", "updated_at": "2026-09-01T10:00:00Z" }),
            json!({ "id": PARENT, "thread_name": "Fix the bug", "updated_at": "2026-09-01T10:09:00Z" })),
    )
    .unwrap();
    Home(root)
}

fn texts(session: &Session, role: Role) -> Vec<String> {
    let msgs = session.messages.iter().filter(|m| m.role == role);
    msgs.flat_map(|m| m.parts.iter().filter_map(Part::as_text).map(str::to_string)).collect()
}

fn calls(session: &Session) -> Vec<(String, Value)> {
    let parts = session.messages.iter().flat_map(|m| m.parts.iter());
    parts
        .filter_map(|p| match p {
            Part::ToolCall { name, input, .. } => Some((name.clone(), input.clone().unwrap_or_default())),
            _ => None,
        })
        .collect()
}

fn export(session: &Session, dir: &Path, name: &str) -> Value {
    let path = dir.join(name);
    Opencode::write(session, &path).unwrap();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn reads_a_desktop_thread_with_its_window_subagent_and_fork() {
    let home = home();
    let rollout = home.0.join(format!("sessions/2026/09/01/rollout-2026-09-01T10-05-00-{PARENT}_{WINDOW}.jsonl"));
    let parent = Codex::read(&rollout).unwrap();
    assert_eq!(parent.source_id, PARENT, "any window reads the whole thread");
    assert_eq!(parent.title, "Fix the bug", "the latest session_index name");
    assert_eq!(parent.directory.as_deref(), Some("/Volumes/projects/app"));
    assert_eq!(texts(&parent, Role::User), ["fix the bug", "and now?"], "injected context is not the user's");
    assert!(parent.messages[0].parts.iter().any(|p| matches!(p, Part::Attachment { mime, data: Some(_), .. } if mime == "image/png")));
    let names: Vec<String> = calls(&parent).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["update_plan", "exec_command", "apply_patch", "spawn_agent"], "a script gives way to what it did");
    let patch = calls(&parent)[2].1["patchText"].as_str().unwrap().to_string();
    assert!(patch.contains("*** Add File: /Volumes/projects/app/new.txt\n+hello\n+world\n"));
    assert!(patch.contains("*** Update File: /Volumes/projects/app/a.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"));
    assert_eq!(calls(&parent)[3].1["message"], "[encrypted by Codex]");
    assert!(texts(&parent, Role::Assistant).iter().any(|t| t == "later"), "the second window is read too");
    let summary = parent.messages.iter().find(|m| m.summary).expect("compaction summary");
    assert!(summary.parts[0].as_text().unwrap().contains("- fix the bug"));
    assert!(summary.parts[0].as_text().unwrap().contains("done"));
    let models: Vec<&str> = parent.messages.iter().filter_map(|m| m.model.as_ref()).map(|m| m.id.as_str()).collect();
    assert_eq!((models.first().copied(), models.last().copied()), (Some("gpt-5.6-sol"), Some("gpt-5.6-luna")));
    assert_eq!(parent.children, [CHILD]);

    let child = Codex::read(&home.0.join(format!("sessions/2026/09/01/rollout-2026-09-01T10-01-00-{CHILD}.jsonl"))).unwrap();
    assert_eq!(child.parent.as_deref(), Some(PARENT));
    assert_eq!(child.title, "child (Explorer)");
    assert_eq!(texts(&child, Role::User), ["Message Type: NEW_TASK\nPayload:\n[encrypted by Codex]"], "the parent's copied history is dropped");

    let fork = Codex::read(&home.0.join(format!("archived_sessions/rollout-2026-09-01T11-00-00-{FORK}.jsonl"))).unwrap();
    assert_eq!(texts(&fork, Role::User), ["fix the bug", "fork prompt"], "a fork starts with the history it was forked from");
    assert!(fork.parent.is_none());

    let meta = info(&home.0.join(format!("sessions/2026/09/01/rollout-2026-09-01T10-01-00-{CHILD}.jsonl"))).unwrap();
    assert_eq!((meta["source"].as_str(), meta["parent_thread_id"].as_str(), meta["primary"].as_bool()), (Some("subagent"), Some(PARENT), Some(true)));
    assert_eq!(info(&rollout).unwrap()["primary"], false);

    let parent_export = export(&parent, &home.0, "parent.json");
    let child_export = export(&child, &home.0, "child.json");
    let tools: Vec<&Value> = parent_export["messages"].as_array().unwrap().iter()
        .flat_map(|m| m["parts"].as_array().unwrap()).filter(|p| p["type"] == "tool").collect();
    let tool = |name: &str| *tools.iter().find(|t| t["tool"] == name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(tool("bash")["state"]["metadata"]["exit"], 2);
    assert_eq!(tool("bash")["state"]["metadata"]["output"], "a.txt\n");
    assert_eq!(tool("bash")["state"]["input"]["workdir"], "/Volumes/projects/app");
    let files = tool("apply_patch")["state"]["metadata"]["files"].as_array().unwrap();
    assert_eq!((files[0]["type"].as_str(), files[0]["additions"].as_u64()), (Some("add"), Some(2)));
    assert_eq!((files[1]["relativePath"].as_str(), files[1]["deletions"].as_u64()), (Some("a.txt"), Some(1)));
    assert_eq!(tool("todowrite")["state"]["metadata"]["todos"][0]["content"], "Fix it");
    assert_eq!(tool("task")["state"]["metadata"]["sessionId"], child_export["info"]["id"]);
    assert_eq!(child_export["info"]["parentID"], parent_export["info"]["id"]);
}
