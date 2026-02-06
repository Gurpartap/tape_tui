mod fixture;

use pi_tui::core::output::TerminalCmd;
use pi_tui::core::terminal_image::is_image_line;
use pi_tui::render::overlay::{
    composite_overlays, resolve_overlay_layout, OverlayAnchor, OverlayOptions, RenderedOverlay,
    SizeValue,
};
use pi_tui::runtime::ime::{
    extract_cursor_position, position_hardware_cursor, CursorPos, CURSOR_MARKER,
};

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

#[test]
fn cursor_marker_and_hardware_cursor_match_fixture() {
    let expected = fixture::read_unescaped("cursor_output.txt");
    let mut lines = vec!["hello".to_string(), format!("wor{CURSOR_MARKER}ld")];
    let pos = extract_cursor_position(&mut lines, 2);
    assert_eq!(pos, Some(CursorPos { row: 1, col: 3 }));
    assert_eq!(lines[1], "world");

    let (new_row, cmds) = position_hardware_cursor(pos, lines.len(), 0, true);
    assert_eq!(new_row, 1);
    let output = cmds_to_bytes(cmds);
    assert_eq!(output, expected);
}

#[test]
fn overlay_composite_anchor_fixture() {
    let expected = fixture::read_lines_unescaped("overlay_composite_anchor.txt");
    let term_width = 10;
    let term_height = 5;

    let mut options = OverlayOptions::default();
    options.width = Some(SizeValue::absolute(3));
    options.anchor = Some(OverlayAnchor::BottomRight);

    let layout = resolve_overlay_layout(Some(&options), 1, term_width, term_height);
    let overlays = vec![RenderedOverlay {
        lines: vec!["\x1b[31mX\x1b[0m".to_string()],
        row: layout.row,
        col: layout.col,
        width: layout.width,
    }];

    let base = vec!["base".to_string()];
    let composed = composite_overlays(base, &overlays, term_width, term_height, 1, is_image_line);

    assert_eq!(composed, expected);
}
