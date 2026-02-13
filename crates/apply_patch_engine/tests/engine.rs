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
