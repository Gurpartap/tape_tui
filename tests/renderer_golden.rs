mod fixture;

use pi_tui::core::output::TerminalCmd;
use pi_tui::core::terminal_image::is_image_line;
use pi_tui::render::renderer::DiffRenderer;

fn cmds_to_bytes(cmds: Vec<TerminalCmd>) -> String {
    let mut out = String::new();
    for cmd in cmds {
        match cmd {
            TerminalCmd::Bytes(data) => out.push_str(&data),
            TerminalCmd::BytesStatic(data) => out.push_str(data),
            TerminalCmd::HideCursor => out.push_str("\x1b[?25l"),
            TerminalCmd::ShowCursor => out.push_str("\x1b[?25h"),
            TerminalCmd::MoveUp(n) => {
                if n > 0 {
                    out.push_str(&format!("\x1b[{n}A"));
                }
            }
            TerminalCmd::MoveDown(n) => {
                if n > 0 {
                    out.push_str(&format!("\x1b[{n}B"));
                }
            }
            TerminalCmd::ColumnAbs(n) => {
                if n > 0 {
                    out.push_str(&format!("\x1b[{n}G"));
                }
            }
            TerminalCmd::BracketedPasteEnable => out.push_str("\x1b[?2004h"),
            TerminalCmd::BracketedPasteDisable => out.push_str("\x1b[?2004l"),
            TerminalCmd::KittyQuery => out.push_str("\x1b[?u"),
            TerminalCmd::KittyEnable => out.push_str("\x1b[>7u"),
            TerminalCmd::KittyDisable => out.push_str("\x1b[<u"),
            TerminalCmd::QueryCellSize => out.push_str("\x1b[16t"),
        }
    }
    out
}

fn not_image(_: &str) -> bool {
    false
}

#[test]
fn golden_first_render() {
    let expected = fixture::read_unescaped("renderer_first_render.txt");
    let mut renderer = DiffRenderer::new();
    let output =
        cmds_to_bytes(renderer.render(vec!["hello".to_string()], 10, 5, not_image, false, false));
    assert_eq!(output, expected);
}

#[test]
fn golden_width_change_full_clear() {
    let expected = fixture::read_unescaped("renderer_width_change_clear.txt");
    let mut renderer = DiffRenderer::new();
    renderer.render(vec!["hello".to_string()], 10, 5, not_image, false, false);

    let output =
        cmds_to_bytes(renderer.render(vec!["hello".to_string()], 12, 5, not_image, false, false));
    assert_eq!(output, expected);
}

#[test]
fn golden_diff_one_line() {
    let expected = fixture::read_unescaped("renderer_diff_one_line.txt");
    let mut renderer = DiffRenderer::new();
    renderer.render(
        vec!["one".to_string(), "two".to_string()],
        20,
        5,
        not_image,
        false,
        false,
    );

    let output = cmds_to_bytes(renderer.render(
        vec!["one".to_string(), "tWO".to_string()],
        20,
        5,
        not_image,
        false,
        false,
    ));
    assert_eq!(output, expected);
}

#[test]
fn golden_clear_on_shrink() {
    let expected = fixture::read_unescaped("renderer_clear_on_shrink.txt");
    let mut renderer = DiffRenderer::new();
    renderer.render(
        vec!["one".to_string(), "two".to_string()],
        20,
        5,
        not_image,
        true,
        false,
    );

    let output =
        cmds_to_bytes(renderer.render(vec!["one".to_string()], 20, 5, not_image, true, false));
    assert_eq!(output, expected);
}

#[test]
fn golden_image_line_bypass() {
    let expected = fixture::read_unescaped("renderer_image_line.txt");
    let mut renderer = DiffRenderer::new();
    renderer.render(vec!["short".to_string()], 5, 5, is_image_line, false, false);

    let output = cmds_to_bytes(renderer.render(
        vec!["\x1b_G1234567890".to_string()],
        5,
        5,
        is_image_line,
        false,
        false,
    ));
    assert_eq!(output, expected);
}
