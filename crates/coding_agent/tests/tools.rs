use std::fs;
use std::path::Path;

use coding_agent::tools::{BuiltinToolExecutor, ToolCall, ToolExecutor};
use tempfile::tempdir;

fn new_executor(workspace_root: &Path) -> BuiltinToolExecutor {
    BuiltinToolExecutor::new(workspace_root).expect("workspace root should be valid")
}

#[test]
fn all_five_tools_have_success_paths() {
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

    let apply_patch_result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Update File: notes/hello.txt\n@@\n-hello world\n+hello patched\n*** End Patch"
            .to_string(),
    });
    assert!(
        apply_patch_result.ok,
        "apply_patch should succeed: {}",
        apply_patch_result.content
    );
    assert!(
        apply_patch_result.content.contains("M notes/hello.txt"),
        "{}",
        apply_patch_result.content
    );

    let patched_result = executor.execute(ToolCall::ReadFile {
        path: "notes/hello.txt".to_string(),
    });
    assert!(patched_result.ok);
    assert_eq!(patched_result.content, "hello patched\n");

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

#[test]
fn apply_patch_fails_with_explicit_parse_error_on_malformed_patch() {
    let workspace = tempdir().expect("temp workspace");
    let mut executor = new_executor(workspace.path());

    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Add File: broken.txt\n+oops".to_string(),
    });

    assert!(!result.ok);
    assert!(
        result.content.contains("apply_patch parse error"),
        "{}",
        result.content
    );
}

#[test]
fn apply_patch_rejects_path_escape_outside_workspace_without_mutating_workspace_files() {
    let outer = tempdir().expect("outer tempdir");
    let workspace_root = outer.path().join("workspace");
    fs::create_dir_all(&workspace_root).expect("create workspace root");

    let tracked_file = workspace_root.join("tracked.txt");
    fs::write(&tracked_file, "safe\n").expect("seed tracked file");

    let mut executor = new_executor(&workspace_root);
    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Update File: tracked.txt\n@@\n-safe\n+unsafe\n*** Add File: ../escape.txt\n+forbidden\n*** End Patch"
            .to_string(),
    });

    assert!(!result.ok);
    assert!(
        result.content.contains("apply_patch path escape rejected"),
        "{}",
        result.content
    );

    assert_eq!(
        fs::read_to_string(&tracked_file).expect("read tracked file"),
        "safe\n"
    );
    assert!(
        !outer.path().join("escape.txt").exists(),
        "path escape should not create a file outside workspace"
    );
}

#[test]
fn apply_patch_context_mismatch_is_non_mutating_even_with_prior_valid_hunks() {
    let workspace = tempdir().expect("temp workspace");
    let file_path = workspace.path().join("context.txt");
    fs::write(&file_path, "line-1\nline-2\n").expect("write seed file");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Update File: context.txt\n@@\n-line-1\n+line-one\n*** Update File: context.txt\n@@\n-missing-line\n+replacement\n*** End Patch"
            .to_string(),
    });

    assert!(!result.ok);
    assert!(
        result.content.contains("apply_patch context mismatch"),
        "{}",
        result.content
    );
    assert_eq!(
        fs::read_to_string(&file_path).expect("read context file"),
        "line-1\nline-2\n"
    );
}

#[test]
fn apply_patch_preserves_order_for_same_file_multi_hunk_updates() {
    let workspace = tempdir().expect("temp workspace");
    let file_path = workspace.path().join("ordered.txt");
    fs::write(&file_path, "alpha\nbeta\n").expect("seed ordered file");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Update File: ordered.txt\n@@\n-alpha\n+ALPHA\n*** Update File: ordered.txt\n@@\n-ALPHA\n+ALPHA!\n*** End Patch"
            .to_string(),
    });

    assert!(result.ok, "{}", result.content);
    assert_eq!(
        fs::read_to_string(&file_path).expect("read ordered file"),
        "ALPHA!\nbeta\n"
    );
}

#[test]
fn apply_patch_preserves_move_then_follow_up_update_order() {
    let workspace = tempdir().expect("temp workspace");
    let source_path = workspace.path().join("old.txt");
    let destination_path = workspace.path().join("moved.txt");
    fs::write(&source_path, "start\n").expect("seed source file");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-start\n+middle\n*** Update File: moved.txt\n@@\n-middle\n+done\n*** End Patch"
            .to_string(),
    });

    assert!(result.ok, "{}", result.content);
    assert!(!source_path.exists(), "source should be removed after move");
    assert_eq!(
        fs::read_to_string(&destination_path).expect("read moved file"),
        "done\n"
    );
}

#[test]
fn apply_patch_io_failure_reports_partial_mutation_when_writes_started() {
    let workspace = tempdir().expect("temp workspace");
    let source_path = workspace.path().join("source.txt");
    fs::write(&source_path, "before\n").expect("seed update source");
    fs::create_dir_all(workspace.path().join("dir")).expect("create directory destination");

    let mut executor = new_executor(workspace.path());
    let result = executor.execute(ToolCall::ApplyPatch {
        input: "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** Update File: source.txt\n*** Move to: dir\n@@\n-before\n+after\n*** End Patch"
            .to_string(),
    });

    assert!(!result.ok);
    assert!(
        result
            .content
            .contains("apply_patch io failure while writing"),
        "{}",
        result.content
    );
    assert!(
        result
            .content
            .contains("apply_patch warning: patch may be partially applied"),
        "{}",
        result.content
    );
    assert!(
        workspace.path().join("created.txt").exists(),
        "first mutation should remain on disk when later IO fails"
    );
}
