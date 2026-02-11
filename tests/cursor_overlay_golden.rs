mod fixture;

use pi_tui::core::output::TerminalCmd;
use pi_tui::core::terminal_image::is_image_line;
use pi_tui::render::overlay::{
    composite_overlays, resolve_overlay_layout, OverlayAnchor, OverlayOptions, RenderedOverlay,
    SizeValue,
};
use pi_tui::runtime::ime::position_hardware_cursor;
use pi_tui::{core::cursor::CursorPos, render::Frame};

fn cmds_to_bytes(cmds: Vec<TerminalCmd>) -> String {
    let mut out = String::new();
    for cmd in cmds {
        match cmd {
            TerminalCmd::Bytes(data) => out.push_str(&data),
            TerminalCmd::BytesStatic(data) => out.push_str(data),
            TerminalCmd::HideCursor => out.push_str("\x1b[?25l"),
            TerminalCmd::ShowCursor => out.push_str("\x1b[?25h"),
            TerminalCmd::ClearLine => out.push_str("\x1b[K"),
            TerminalCmd::ClearFromCursor => out.push_str("\x1b[J"),
            TerminalCmd::ClearScreen => out.push_str("\x1b[2J\x1b[H"),
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
fn cursor_metadata_and_hardware_cursor_match_fixture() {
    let expected = fixture::read_unescaped("cursor_output.txt");
    let lines = vec!["hello".to_string(), "world".to_string()];
    let pos = Some(CursorPos { row: 1, col: 3 });
    let frame = Frame::from(lines.clone()).with_cursor(pos);
    assert_eq!(frame.cursor(), pos);
    let total_lines = frame.lines().len();
    let rendered = frame.into_strings();
    assert_eq!(rendered, lines);

    let (new_row, cmds) = position_hardware_cursor(pos, total_lines, 0, true);
    assert_eq!(new_row, 1);
    let output = cmds_to_bytes(cmds);
    assert_eq!(output, expected);
}

#[test]
fn overlay_composite_anchor_fixture() {
    let expected = fixture::read_lines_unescaped("overlay_composite_anchor.txt");
    let term_width = 10;
    let term_height = 5;

    let options = OverlayOptions {
        width: Some(SizeValue::absolute(3)),
        anchor: Some(OverlayAnchor::BottomRight),
        ..Default::default()
    };

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

#[test]
fn overlay_mixed_ansi_osc_fixture() {
    let expected = fixture::read_lines_unescaped("overlay_mixed_ansi_osc.txt");
    let term_width = 10;
    let term_height = 2;
    let overlays = vec![RenderedOverlay {
        lines: vec!["\x1b[31mAB\x1b]8;;https://x\x07CDEFGH\x1b]8;;\x07\x1b[0m".to_string()],
        row: 0,
        col: 2,
        width: 6,
    }];
    let base = vec!["0123456789".to_string(), "INPUT".to_string()];
    let composed = composite_overlays(base, &overlays, term_width, term_height, 2, is_image_line);

    assert_eq!(composed, expected);
    assert_eq!(
        composed[0].matches("\x1b[0m\x1b]8;;\x07").count(),
        2,
        "overlay composition must bracket inserted segments with reset guards"
    );
    assert_eq!(composed[1], "INPUT");
    assert!(
        !composed[1].contains('\x1b'),
        "base lines after a composited overlay should remain unstyled"
    );
}
