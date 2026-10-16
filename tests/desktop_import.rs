#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("baton-desktop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("input.jsonl"), concat!(
            "{\"type\":\"user\",\"sessionId\":\"external-id\",\"message\":{\"role\":\"user\",\"content\":\"Fixture conversation\"},\"timestamp\":\"2026-01-01T00:00:00Z\"}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"external-id\",\"message\":{\"role\":\"assistant\",\"content\":\"Fixture reply\"},\"timestamp\":\"2026-01-01T00:00:01Z\"}\n"
        )).unwrap();
        Self(root)
    }

    fn scope(&self, account: &str, org: &str) -> PathBuf {
        let path = self
            .0
            .join("Library/Application Support/Claude/claude-code-sessions")
            .join(account)
            .join(org);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn convert(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .env("HOME", &self.0)
            .current_dir(&self.0)
            .args([
                "convert",
                "--from",
                "claude-code",
                "--to",
                "claude-code",
                "input.jsonl",
                "--output",
                "output.jsonl",
            ])
            .args(extra)
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
#[cfg(target_os = "macos")]
fn registers_in_every_scope_when_claude_code_is_imported() {
    // Given two account/org scopes, including one with scheduled tasks only.
    let home = Sandbox::new();
    let first = home.scope(
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    );
    let second = home.scope(
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
    );
    std::fs::create_dir(second.join("scheduled-tasks")).unwrap();
    std::fs::write(first.join("existing.json"), "do not read or modify").unwrap();
    // When importing through the real CLI.
    let output = home.convert(&["--import"]);
    // Then each scope links to the installed transcript, without inherited secrets.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(first.join("existing.json")).unwrap(),
        "do not read or modify"
    );
    for scope in [first, second] {
        let entry = std::fs::read_dir(&scope)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("local_"))
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
        let id = value["cliSessionId"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        let cwd = value["cwd"].as_str().unwrap();
        let encoded: String = cwd
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | '.' | ':') {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        let transcript = home
            .0
            .join(".claude/projects")
            .join(encoded)
            .join(format!("{id}.jsonl"));
        let raw = std::fs::read_to_string(transcript).unwrap();
        for line in raw.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["sessionId"], id);
        }
        assert_eq!(value["originCwd"], value["cwd"]);
        assert_eq!(value["title"], "Fixture conversation");
        assert_eq!(value["isArchived"], false);
        assert_eq!(value["remoteMcpServersConfig"], serde_json::json!([]));
        assert!(value.get("bridgeSessionIds").is_none());
        assert!(value.get("promptAppendSnapshot").is_none());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            entry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn converts_without_registration_when_import_is_missing() {
    // Given an explicit fixture input, when conversion is requested without --import.
    let home = Sandbox::new();
    let output = home.convert(&[]);
    // Then only the converted output is created.
    assert!(output.status.success());
    assert!(home.0.join("output.jsonl").exists());
    assert!(!home.0.join(".claude").exists());
}

#[test]
fn leaves_desktop_untouched_when_only_conversion_is_requested() {
    // Given an existing Desktop scope.
    let home = Sandbox::new();
    let scope = home.scope(
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    );
    // When converting without importing.
    let output = home.convert(&[]);
    // Then no Desktop registration is added.
    assert!(output.status.success());
    assert_eq!(std::fs::read_dir(scope).unwrap().count(), 0);
}

#[test]
fn imports_cli_when_desktop_is_unavailable() {
    // Given no Desktop account/org directories.
    let home = Sandbox::new();
    // When importing normally, then the CLI transcript remains available.
    let output = home.convert(&["--import"]);
    assert!(output.status.success());
    assert!(home.0.join(".claude/projects").is_dir());
}

#[test]
#[cfg(target_os = "macos")]
fn links_existing_transcript_when_source_uuid_is_compact() {
    let home = Sandbox::new();
    let scope = home.scope(
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    );
    let id = "11111111111141118111111111111111";
    std::fs::rename(
        home.0.join("input.jsonl"),
        home.0.join(format!("{id}.jsonl")),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_baton"))
        .env("HOME", &home.0)
        .current_dir(&home.0)
        .args([
            "convert",
            "--from",
            "claude-code",
            "--to",
            "claude-code",
            &format!("{id}.jsonl"),
            "--output",
            "output.jsonl",
            "--import",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry = std::fs::read_dir(scope).unwrap().next().unwrap().unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
    assert_eq!(value["cliSessionId"], id);
    let project = std::fs::read_dir(home.0.join(".claude/projects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(project.path().join(format!("{id}.jsonl")).is_file());
}
