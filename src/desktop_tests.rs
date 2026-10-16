use super::*;
use crate::canonical::Agent;

fn session() -> Session {
    Session {
        source_id: "fixture".into(),
        origin: Agent::ClaudeCode,
        title: "Fixture".into(),
        time_created: 1_700_000_000_000,
        time_updated: 1_700_000_001_000,
        directory: None,
        messages: Vec::new(),
    }
}

#[test]
fn preserves_existing_entry_when_registration_collides() {
    // Given a registration path that already contains unrelated data.
    let temp = tempfile::tempdir().unwrap();
    let session = session();
    let entry = Registration::new(
        "11111111-1111-4111-8111-111111111111",
        temp.path(),
        &session,
    )
    .unwrap();
    let path = temp.path().join(format!("{}.json", entry.session_id));
    fs::write(&path, "private existing content").unwrap();
    // When registration encounters that exact path.
    let created = register(&[temp.path().to_path_buf()], &entry).unwrap();
    // Then it skips the file without replacing it or leaving temporary files.
    assert_eq!(created, 0);
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "private existing content"
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn includes_empty_and_scheduled_scopes_when_discovering_accounts() {
    // Given multiple UUID scopes, unrelated directories, and symlinks.
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join(Uuid::new_v4().to_string());
    let first = account.join(Uuid::new_v4().to_string());
    let second = account.join(Uuid::new_v4().to_string());
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(second.join("scheduled-tasks")).unwrap();
    fs::create_dir_all(account.join("not-an-org")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), account.join(Uuid::new_v4().to_string())).unwrap();
    std::os::unix::fs::symlink(&account, temp.path().join(Uuid::new_v4().to_string())).unwrap();
    // When discovering eligible directories.
    let mut scopes = account_scopes(temp.path()).unwrap();
    // Then only real account/org directories are included, regardless of their contents.
    scopes.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(scopes, expected);
}

#[test]
fn refuses_symlink_when_registration_root_is_redirected() {
    // Given a root pointing outside the requested Desktop directory.
    let temp = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::os::unix::fs::symlink(target.path(), &root).unwrap();
    // When finding account scopes, then the redirected root is rejected.
    assert!(account_scopes(&root).is_err());
}

#[test]
fn uses_fresh_uuid4_and_safe_defaults_when_building_registration() {
    // Given an imported session with known transcript timestamps.
    let session = session();
    let id = "11111111-1111-1111-8111-111111111111";
    // When building Desktop metadata.
    let entry = Registration::new(id, Path::new("/fixture"), &session).unwrap();
    // Then the Desktop ID is UUID4, the CLI ID stays intact, and times are epoch ms.
    let desktop_id = Uuid::parse_str(entry.session_id.strip_prefix("local_").unwrap()).unwrap();
    assert_eq!(desktop_id.get_version_num(), 4);
    assert_eq!(entry.cli_session_id.to_string(), id);
    assert_eq!(entry.created_at, 1_700_000_000_000);
    assert_eq!(entry.last_activity_at, 1_700_000_001_000);
    assert_eq!(entry.permission_mode, "default");
    assert_eq!(entry.chrome_permission_mode, "ask");
    assert!(entry.remote_mcp_servers_config.is_empty());
}

#[test]
fn preserves_cli_filename_spelling_when_uuid_is_not_hyphenated() {
    let session = session();
    let id = "11111111111141118111111111111111";
    let entry = Registration::new(id, Path::new("/fixture"), &session).unwrap();
    assert_eq!(serde_json::to_value(entry).unwrap()["cliSessionId"], id);
}

#[test]
fn skips_registration_when_desktop_directory_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let scopes = account_scopes(&temp.path().join("absent")).unwrap();
    assert!(scopes.is_empty());
}

#[test]
fn skips_registration_when_account_has_no_org_directories() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(Uuid::new_v4().to_string())).unwrap();
    let scopes = account_scopes(temp.path()).unwrap();
    assert!(scopes.is_empty());
}
