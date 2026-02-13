use apply_patch_engine::apply_patch;
use apply_patch_engine::parse_patch;

use tempfile::tempdir;

#[test]
fn valid_patch_parse_and_apply() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("hello.txt");
    std::fs::write(&file, "hello\nworld\n").expect("seed file");

    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n hello\n-world\n+rust\n*** End Patch",
        file.display()
    );

    parse_patch(&patch).expect("patch parses");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch(&patch, &mut stdout, &mut stderr).expect("patch applies");

    assert_eq!(
        std::fs::read_to_string(file).expect("read"),
        "hello\nrust\n"
    );
    assert_eq!(String::from_utf8(stderr).expect("utf8"), "");
}

#[test]
fn malformed_patch_fails_parse() {
    let err =
        parse_patch("*** Begin Patch\n*** Add File: foo\n+bad").expect_err("parse should fail");
    let msg = err.to_string();
    assert!(msg.contains("invalid patch"));
}

#[test]
fn context_mismatch_fails_apply() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("context.txt");
    std::fs::write(&file, "a\nb\n").expect("seed file");

    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-missing\n+present\n*** End Patch",
        file.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let err = apply_patch(&patch, &mut stdout, &mut stderr).expect_err("apply should fail");

    let message = err.to_string();
    assert!(message.contains("Failed to find expected lines"));
}

#[test]
fn add_delete_update_paths() {
    let dir = tempdir().expect("tempdir");
    let update_file = dir.path().join("update.txt");
    let delete_file = dir.path().join("delete.txt");
    let add_file = dir.path().join("add.txt");

    std::fs::write(&update_file, "old\n").expect("seed update");
    std::fs::write(&delete_file, "gone\n").expect("seed delete");

    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+new\n*** Delete File: {}\n*** Update File: {}\n@@\n-old\n+newer\n*** End Patch",
        add_file.display(),
        delete_file.display(),
        update_file.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch(&patch, &mut stdout, &mut stderr).expect("apply");

    assert_eq!(
        std::fs::read_to_string(add_file).expect("read add"),
        "new\n"
    );
    assert!(!delete_file.exists());
    assert_eq!(
        std::fs::read_to_string(update_file).expect("read update"),
        "newer\n"
    );
    assert_eq!(String::from_utf8(stderr).expect("utf8"), "");
}

#[test]
fn multiple_operations_emit_deterministic_summary_order() {
    let dir = tempdir().expect("tempdir");
    let modify_file = dir.path().join("modify.txt");
    let delete_file = dir.path().join("delete.txt");
    let add_file = dir.path().join("nested/new.txt");

    std::fs::write(&modify_file, "line1\nline2\n").expect("seed modify");
    std::fs::write(&delete_file, "obsolete\n").expect("seed delete");

    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+created\n*** Delete File: {}\n*** Update File: {}\n@@\n-line2\n+changed\n*** End Patch",
        add_file.display(),
        delete_file.display(),
        modify_file.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch(&patch, &mut stdout, &mut stderr).expect("apply");

    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        format!(
            "Success. Updated the following files:\nA {}\nM {}\nD {}\n",
            add_file.display(),
            modify_file.display(),
            delete_file.display()
        )
    );
    assert_eq!(String::from_utf8(stderr).expect("utf8"), "");
    assert_eq!(
        std::fs::read_to_string(&add_file).expect("read add"),
        "created\n"
    );
    assert_eq!(
        std::fs::read_to_string(&modify_file).expect("read modify"),
        "line1\nchanged\n"
    );
    assert!(!delete_file.exists());
}

#[test]
fn move_overwrites_existing_destination_path() {
    let dir = tempdir().expect("tempdir");
    let source_path = dir.path().join("old/name.txt");
    let destination_path = dir.path().join("renamed/dir/name.txt");

    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("mkdir src");
    std::fs::create_dir_all(destination_path.parent().expect("dest parent")).expect("mkdir dst");
    std::fs::write(&source_path, "from\n").expect("seed source");
    std::fs::write(&destination_path, "existing\n").expect("seed destination");

    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-from\n+new\n*** End Patch",
        source_path.display(),
        destination_path.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch(&patch, &mut stdout, &mut stderr).expect("apply");

    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        format!(
            "Success. Updated the following files:\nM {}\n",
            destination_path.display()
        )
    );
    assert_eq!(String::from_utf8(stderr).expect("utf8"), "");
    assert!(
        !source_path.exists(),
        "source file should be removed after move"
    );
    assert_eq!(
        std::fs::read_to_string(&destination_path).expect("read destination"),
        "new\n"
    );
}

#[test]
fn update_file_appends_trailing_newline_when_missing() {
    let dir = tempdir().expect("tempdir");
    let target_path = dir.path().join("no_newline.txt");
    std::fs::write(&target_path, "no newline at end").expect("seed file without trailing newline");

    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-no newline at end\n+first line\n+second line\n*** End Patch",
        target_path.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    apply_patch(&patch, &mut stdout, &mut stderr).expect("apply");

    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        format!(
            "Success. Updated the following files:\nM {}\n",
            target_path.display()
        )
    );
    assert_eq!(String::from_utf8(stderr).expect("utf8"), "");
    assert_eq!(
        std::fs::read_to_string(&target_path).expect("read target"),
        "first line\nsecond line\n"
    );
}

#[test]
fn failure_after_partial_success_leaves_earlier_changes_on_disk() {
    let dir = tempdir().expect("tempdir");
    let created_path = dir.path().join("created.txt");
    let missing_path = dir.path().join("missing.txt");

    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+hello\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
        created_path.display(),
        missing_path.display()
    );

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let error = apply_patch(&patch, &mut stdout, &mut stderr).expect_err("apply should fail");

    assert!(error.to_string().contains("Failed to read file to update"));
    assert_eq!(String::from_utf8(stdout).expect("utf8"), "");
    assert_eq!(
        String::from_utf8(stderr).expect("utf8"),
        format!(
            "Failed to read file to update {}: No such file or directory (os error 2)\n",
            missing_path.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&created_path).expect("created file should remain"),
        "hello\n"
    );
}
