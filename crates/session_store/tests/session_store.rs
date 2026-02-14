use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use agent_provider::RunMessage;
use serde_json::json;
use session_store::{
    session_root, SessionEntry, SessionEntryKind, SessionHeader, SessionStore, SessionStoreError,
};
use tempfile::TempDir;

fn write_session_file(lines: &[String]) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let path = dir.path().join("session.jsonl");
    let mut file = File::create(&path).expect("session file should be created");

    for line in lines {
        writeln!(file, "{line}").expect("line should be written");
    }

    (dir, path)
}

fn write_empty_session_file() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let path = dir.path().join("session.jsonl");
    File::create(&path).expect("empty session file should be created");
    (dir, path)
}

fn header_line(cwd: &Path) -> String {
    json!({
        "type": "session",
        "version": 1,
        "session_id": "session-1",
        "created_at": "2026-02-14T00:00:00Z",
        "cwd": cwd.display().to_string(),
    })
    .to_string()
}

fn user_entry_line(id: &str, parent_id: Option<&str>, ts: &str, text: &str) -> String {
    json!({
        "type": "entry",
        "id": id,
        "parent_id": parent_id,
        "ts": ts,
        "kind": "user_text",
        "text": text,
    })
    .to_string()
}

fn assistant_entry_line(id: &str, parent_id: Option<&str>, ts: &str, text: &str) -> String {
    json!({
        "type": "entry",
        "id": id,
        "parent_id": parent_id,
        "ts": ts,
        "kind": "assistant_text",
        "text": text,
    })
    .to_string()
}

#[test]
fn open_rejects_missing_header() {
    let (_dir, path) = write_empty_session_file();

    let error = SessionStore::open(&path)
        .err()
        .expect("empty file must fail");
    assert!(matches!(error, SessionStoreError::MissingHeader { .. }));
}

#[test]
fn open_rejects_non_header_first_line() {
    let (_dir, path) = write_session_file(&[user_entry_line(
        "entry-1",
        None,
        "2026-02-14T00:00:01Z",
        "hello",
    )]);

    let error = SessionStore::open(&path)
        .err()
        .expect("entry as first line must fail");
    assert!(matches!(
        error,
        SessionStoreError::InvalidHeaderRecord { line: 1, .. }
    ));
}

#[test]
fn open_rejects_unsupported_header_version() {
    let (_dir, path) = write_session_file(&[json!({
        "type": "session",
        "version": 2,
        "session_id": "session-1",
        "created_at": "2026-02-14T00:00:00Z",
        "cwd": "/tmp",
    })
    .to_string()]);

    let error = SessionStore::open(&path)
        .err()
        .expect("unsupported version must fail");
    assert!(matches!(
        error,
        SessionStoreError::UnsupportedVersion {
            line: 1,
            found: 2,
            ..
        }
    ));
}

#[test]
fn open_rejects_unknown_header_fields() {
    let (_dir, path) = write_session_file(&[json!({
        "type": "session",
        "version": 1,
        "session_id": "session-1",
        "created_at": "2026-02-14T00:00:00Z",
        "cwd": "/tmp",
        "unexpected": true,
    })
    .to_string()]);

    let error = SessionStore::open(&path)
        .err()
        .expect("unknown header field must fail");
    assert!(matches!(
        error,
        SessionStoreError::JsonLineParse { line: 1, .. }
    ));
}

#[test]
fn open_rejects_malformed_json_line_with_line_context() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let path = temp.path().join("session.jsonl");
    let mut file = File::create(&path).expect("session file should be created");
    writeln!(file, "{}", header_line(temp.path())).expect("header should be written");
    writeln!(file, "{{ this is invalid json").expect("invalid line should be written");

    let error = SessionStore::open(&path)
        .err()
        .expect("malformed json line must fail");
    assert!(matches!(
        error,
        SessionStoreError::JsonLineParse { line: 2, .. }
    ));
}

#[test]
fn open_rejects_unknown_entry_fields() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let lines = vec![
        header_line(temp.path()),
        json!({
            "type": "entry",
            "id": "entry-1",
            "parent_id": null,
            "ts": "2026-02-14T00:00:01Z",
            "kind": "assistant_text",
            "text": "hi",
            "extra": "nope",
        })
        .to_string(),
    ];
    let (_dir, path) = write_session_file(&lines);

    let error = SessionStore::open(&path)
        .err()
        .expect("unknown entry field must fail");
    assert!(matches!(
        error,
        SessionStoreError::JsonLineParse { line: 2, .. }
    ));
}

#[test]
fn open_rejects_unknown_entry_kind() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let lines = vec![
        header_line(temp.path()),
        json!({
            "type": "entry",
            "id": "entry-1",
            "parent_id": null,
            "ts": "2026-02-14T00:00:01Z",
            "kind": "unknown_kind",
            "text": "hi",
        })
        .to_string(),
    ];
    let (_dir, path) = write_session_file(&lines);

    let error = SessionStore::open(&path)
        .err()
        .expect("unknown entry kind must fail");
    assert!(matches!(
        error,
        SessionStoreError::JsonLineParse { line: 2, .. }
    ));
}

#[test]
fn open_rejects_duplicate_entry_id() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let lines = vec![
        header_line(temp.path()),
        user_entry_line("entry-1", None, "2026-02-14T00:00:01Z", "first"),
        assistant_entry_line(
            "entry-1",
            Some("entry-1"),
            "2026-02-14T00:00:02Z",
            "duplicate",
        ),
    ];
    let (_dir, path) = write_session_file(&lines);

    let error = SessionStore::open(&path)
        .err()
        .expect("duplicate ids must fail");
    assert!(matches!(
        error,
        SessionStoreError::DuplicateEntryId { line: 3, .. }
    ));
}

#[test]
fn open_rejects_dangling_parent_id() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let lines = vec![
        header_line(temp.path()),
        assistant_entry_line(
            "entry-1",
            Some("missing"),
            "2026-02-14T00:00:01Z",
            "dangling",
        ),
    ];
    let (_dir, path) = write_session_file(&lines);

    let error = SessionStore::open(&path)
        .err()
        .expect("dangling parent id must fail");
    assert!(matches!(
        error,
        SessionStoreError::DanglingParentId { line: 2, .. }
    ));
}

#[test]
fn open_sets_current_leaf_from_append_order() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let lines = vec![
        header_line(temp.path()),
        user_entry_line("entry-1", None, "2026-02-14T00:00:01Z", "hello"),
        assistant_entry_line("entry-2", Some("entry-1"), "2026-02-14T00:00:02Z", "world"),
    ];
    let (_dir, path) = write_session_file(&lines);

    let store = SessionStore::open(&path).expect("valid session file should open");
    assert_eq!(store.current_leaf_id(), Some("entry-2"));
}

#[test]
fn create_new_uses_cwd_agent_sessions_root_and_writes_header() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");

    let expected_root = cwd_dir.path().join(".agent").join("sessions");
    assert!(store.path().starts_with(&expected_root));

    let file = std::fs::read_to_string(store.path()).expect("session file should be readable");
    let mut lines = file.lines();
    let header_line = lines.next().expect("header line should exist");
    let parsed_header: SessionHeader =
        serde_json::from_str(header_line).expect("header should deserialize");

    assert_eq!(parsed_header.version, 1);
    assert_eq!(parsed_header.session_id, store.header().session_id);
    assert_eq!(parsed_header.created_at, store.header().created_at);
    assert_eq!(parsed_header.cwd, cwd_dir.path().display().to_string());
    assert!(lines.next().is_none());
}

#[test]
fn create_new_fails_when_session_root_is_unwritable() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let blocked_agent_path = cwd_dir.path().join(".agent");
    std::fs::write(&blocked_agent_path, "file blocks directory creation")
        .expect("blocker file should be created");

    let error = SessionStore::create_new(cwd_dir.path())
        .err()
        .expect("create_new should fail when session root cannot be created");

    assert!(matches!(error, SessionStoreError::Io { .. }));
}

#[test]
fn append_writes_each_entry_and_updates_leaf() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let mut store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");

    let entry_1 = SessionEntry::new(
        "entry-1",
        None::<String>,
        "2026-02-14T00:00:01Z",
        SessionEntryKind::UserText {
            text: "hello".to_string(),
        },
    );
    store.append(entry_1).expect("first append should succeed");
    assert_eq!(store.current_leaf_id(), Some("entry-1"));

    let after_first_append =
        std::fs::read_to_string(store.path()).expect("session file should be readable");
    assert_eq!(after_first_append.lines().count(), 2);

    let entry_2 = SessionEntry::new(
        "entry-2",
        Some("entry-1"),
        "2026-02-14T00:00:02Z",
        SessionEntryKind::AssistantText {
            text: "world".to_string(),
        },
    );
    store.append(entry_2).expect("second append should succeed");
    assert_eq!(store.current_leaf_id(), Some("entry-2"));

    let reopened = SessionStore::open(store.path()).expect("reopen should succeed");
    assert_eq!(reopened.current_leaf_id(), Some("entry-2"));
}

#[test]
fn append_rejects_invalid_graph_updates() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let mut store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");

    let entry_1 = SessionEntry::new(
        "entry-1",
        None::<String>,
        "2026-02-14T00:00:01Z",
        SessionEntryKind::UserText {
            text: "hello".to_string(),
        },
    );
    store.append(entry_1).expect("first append should succeed");

    let duplicate = SessionEntry::new(
        "entry-1",
        None::<String>,
        "2026-02-14T00:00:02Z",
        SessionEntryKind::AssistantText {
            text: "duplicate".to_string(),
        },
    );
    let duplicate_error = store
        .append(duplicate)
        .expect_err("duplicate entry id should fail append");
    assert!(matches!(
        duplicate_error,
        SessionStoreError::DuplicateEntryId { line: 3, .. }
    ));
    assert_eq!(store.current_leaf_id(), Some("entry-1"));

    let dangling = SessionEntry::new(
        "entry-2",
        Some("missing-parent"),
        "2026-02-14T00:00:02Z",
        SessionEntryKind::AssistantText {
            text: "dangling".to_string(),
        },
    );
    let dangling_error = store
        .append(dangling)
        .expect_err("dangling parent id should fail append");
    assert!(matches!(
        dangling_error,
        SessionStoreError::DanglingParentId { line: 3, .. }
    ));
    assert_eq!(store.current_leaf_id(), Some("entry-1"));
}

#[test]
fn replay_leaf_reconstructs_run_message_sequence() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let mut store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");

    store
        .append(SessionEntry::new(
            "entry-1",
            None::<String>,
            "2026-02-14T00:00:01Z",
            SessionEntryKind::UserText {
                text: "hello".to_string(),
            },
        ))
        .expect("user append should succeed");
    store
        .append(SessionEntry::new(
            "entry-2",
            Some("entry-1"),
            "2026-02-14T00:00:02Z",
            SessionEntryKind::AssistantText {
                text: "working".to_string(),
            },
        ))
        .expect("assistant append should succeed");
    store
        .append(SessionEntry::new(
            "entry-3",
            Some("entry-2"),
            "2026-02-14T00:00:03Z",
            SessionEntryKind::ToolCall {
                call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                arguments: json!({"command": "echo hi"}),
            },
        ))
        .expect("tool call append should succeed");
    store
        .append(SessionEntry::new(
            "entry-4",
            Some("entry-3"),
            "2026-02-14T00:00:04Z",
            SessionEntryKind::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                content: json!({"stdout": "hi"}),
                is_error: false,
            },
        ))
        .expect("tool result append should succeed");

    let replayed = store
        .replay_leaf(None)
        .expect("replay from current leaf should succeed");

    assert_eq!(
        replayed,
        vec![
            RunMessage::UserText {
                text: "hello".to_string(),
            },
            RunMessage::AssistantText {
                text: "working".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                arguments: json!({"command": "echo hi"}),
            },
            RunMessage::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                content: json!({"stdout": "hi"}),
                is_error: false,
            },
        ]
    );
}

#[test]
fn replay_leaf_respects_explicit_target_leaf() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let mut store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");

    store
        .append(SessionEntry::new(
            "entry-1",
            None::<String>,
            "2026-02-14T00:00:01Z",
            SessionEntryKind::UserText {
                text: "root".to_string(),
            },
        ))
        .expect("entry-1 append should succeed");
    store
        .append(SessionEntry::new(
            "entry-2",
            Some("entry-1"),
            "2026-02-14T00:00:02Z",
            SessionEntryKind::AssistantText {
                text: "branch-a".to_string(),
            },
        ))
        .expect("entry-2 append should succeed");
    store
        .append(SessionEntry::new(
            "entry-3",
            Some("entry-1"),
            "2026-02-14T00:00:03Z",
            SessionEntryKind::AssistantText {
                text: "branch-b".to_string(),
            },
        ))
        .expect("entry-3 append should succeed");

    let replayed = store
        .replay_leaf(Some("entry-2"))
        .expect("targeted replay should succeed");

    assert_eq!(
        replayed,
        vec![
            RunMessage::UserText {
                text: "root".to_string(),
            },
            RunMessage::AssistantText {
                text: "branch-a".to_string(),
            },
        ]
    );
}

#[test]
fn replay_leaf_rejects_unknown_leaf_id() {
    let cwd_dir = tempfile::tempdir().expect("tempdir should be created");
    let mut store = SessionStore::create_new(cwd_dir.path()).expect("create_new should succeed");
    store
        .append(SessionEntry::new(
            "entry-1",
            None::<String>,
            "2026-02-14T00:00:01Z",
            SessionEntryKind::UserText {
                text: "hello".to_string(),
            },
        ))
        .expect("append should succeed");

    let error = store
        .replay_leaf(Some("missing-leaf"))
        .expect_err("unknown leaf id must fail replay");
    assert!(matches!(error, SessionStoreError::UnknownLeafId { .. }));
}

#[test]
fn latest_session_path_returns_newest_jsonl_file() {
    let cwd = tempfile::tempdir().expect("tempdir should be created");
    let root = session_root(cwd.path());
    std::fs::create_dir_all(&root).expect("session root should be created");

    let older_path = root.join("2026-02-14T00-00-00Z_older.jsonl");
    std::fs::write(&older_path, "{}").expect("older session file should be written");

    let newer_path = root.join("2026-02-14T00-00-00Z_newer.jsonl");
    std::fs::write(&newer_path, "{}").expect("newer session file should be written");

    let latest =
        SessionStore::latest_session_path(cwd.path()).expect("latest session path should resolve");
    assert_eq!(latest, newer_path);
}

#[test]
fn latest_session_path_errors_when_no_session_files_exist() {
    let cwd = tempfile::tempdir().expect("tempdir should be created");

    let error = SessionStore::latest_session_path(cwd.path())
        .expect_err("missing session root should return explicit no-sessions error");
    assert!(matches!(error, SessionStoreError::NoSessionsFound { .. }));
}
