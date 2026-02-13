use std::fs;
use std::path::Path;

use coding_agent::tools::{BuiltinToolExecutor, ToolCall, ToolExecutor};
use tempfile::tempdir;

fn new_executor(workspace_root: &Path) -> BuiltinToolExecutor {
    BuiltinToolExecutor::new(workspace_root).expect("workspace root should be valid")
}

#[test]
fn all_four_tools_have_success_paths() {
    let workspace = tempdir().expect("temp workspace");
    let mut executor = new_executor(workspace.path());

    let write_result = executor.execute(ToolCall::WriteFile {
        path: "notes/hello.txt".to_string(),
        content: "hello".to_string(),
    });
    assert!(
        write_result.ok,
        "write_file should succeed: {}",
        write_result.content
    );

    let read_result = executor.execute(ToolCall::ReadFile {
        path: "notes/hello.txt".to_string(),
    });
    assert!(
        read_result.ok,
        "read_file should succeed: {}",
        read_result.content
    );
    assert_eq!(read_result.content, "hello");

    let edit_result = executor.execute(ToolCall::EditFile {
        path: "notes/hello.txt".to_string(),
        old_text: "hello".to_string(),
        new_text: "hello world".to_string(),
    });
    assert!(
        edit_result.ok,
        "edit_file should succeed: {}",
        edit_result.content
    );

    let reread_result = executor.execute(ToolCall::ReadFile {
        path: "notes/hello.txt".to_string(),
    });
    assert!(reread_result.ok);
    assert_eq!(reread_result.content, "hello world");

    let bash_result = executor.execute(ToolCall::Bash {
        command: "printf 'bash-ok'".to_string(),
        timeout_sec: None,
        cwd: Some(".".to_string()),
    });
    assert!(
        bash_result.ok,
        "bash should succeed: {}",
        bash_result.content
    );
    assert!(bash_result.content.contains("exit_code=0"));
    assert!(bash_result.content.contains("bash-ok"));
}

#[test]
fn bash_reports_non_zero_exit_as_failure() {
    let workspace = tempdir().expect("temp workspace");
    let mut executor = new_executor(workspace.path());

    let result = executor.execute(ToolCall::Bash {
        command: "echo 'boom' 1>&2; exit 7".to_string(),
        timeout_sec: Some(5),
        cwd: None,
    });

    assert!(!result.ok);
    assert!(result.content.contains("exit_code=7"), "{}", result.content);
    assert!(result.content.contains("boom"), "{}", result.content);
}

#[test]
fn read_file_rejects_path_escape_outside_workspace() {
    let outer = tempdir().expect("outer temp dir");
    let workspace_root = outer.path().join("workspace");
    fs::create_dir_all(&workspace_root).expect("create workspace root");

    let outside_path = outer.path().join("outside.txt");
    fs::write(&outside_path, "outside").expect("write outside file");

    let mut executor = new_executor(&workspace_root);
    let result = executor.execute(ToolCall::ReadFile {
        path: "../outside.txt".to_string(),
    });

    assert!(!result.ok);
    assert!(
        result.content.contains("Path escapes workspace root"),
        "{}",
        result.content
    );
}

#[test]
fn write_file_rejects_path_escape_outside_workspace() {
    let workspace = tempdir().expect("temp workspace");
    let mut executor = new_executor(workspace.path());

    let result = executor.execute(ToolCall::WriteFile {
        path: "../escape.txt".to_string(),
        content: "forbidden".to_string(),
    });

    assert!(!result.ok);
    assert!(
        result.content.contains("Path escapes workspace root"),
        "{}",
        result.content
    );
}

#[test]
fn edit_file_fails_when_old_text_has_no_match() {
    let workspace = tempdir().expect("temp workspace");
    let file_path = workspace.path().join("sample.txt");
    fs::write(&file_path, "abc").expect("write sample file");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::EditFile {
        path: "sample.txt".to_string(),
        old_text: "zzz".to_string(),
        new_text: "x".to_string(),
    });

    assert!(!result.ok);
    assert!(result.content.contains("found 0"), "{}", result.content);
}

#[test]
fn edit_file_fails_when_old_text_has_multiple_matches() {
    let workspace = tempdir().expect("temp workspace");
    let file_path = workspace.path().join("sample.txt");
    fs::write(&file_path, "dup dup").expect("write sample file");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::EditFile {
        path: "sample.txt".to_string(),
        old_text: "dup".to_string(),
        new_text: "x".to_string(),
    });

    assert!(!result.ok);
    assert!(result.content.contains("found 2"), "{}", result.content);
}
