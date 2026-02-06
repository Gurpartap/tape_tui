use std::fs;
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("read_dir({}): {err}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("read_dir entry ({}): {err}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn widgets_do_not_depend_on_render_layer() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let widgets_dir = manifest_dir.join("src/widgets");

    let mut files = Vec::new();
    collect_rs_files(&widgets_dir, &mut files);
    files.sort();

    let mut offenders = Vec::new();
    for file in files {
        let contents = fs::read_to_string(&file).unwrap_or_else(|err| panic!("read_to_string({}): {err}", file.display()));
        if contents.contains("crate::render::")
            || contents.contains("use crate::render")
            || contents.contains("pi_tui::render::")
            || contents.contains("use pi_tui::render")
        {
            offenders.push(file);
        }
    }

    assert!(
        offenders.is_empty(),
        "widgets must depend on `core` only, but found render-layer imports in:\n{}",
        offenders
            .iter()
            .map(|path| {
                path.strip_prefix(&manifest_dir)
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
}

