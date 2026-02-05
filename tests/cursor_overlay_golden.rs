mod fixture;

use pi_tui::core::terminal::Terminal;
use pi_tui::core::terminal_image::is_image_line;
use pi_tui::render::overlay::{
    composite_overlays, resolve_overlay_layout, OverlayAnchor, OverlayOptions, RenderedOverlay,
    SizeValue,
};
use pi_tui::runtime::ime::{extract_cursor_position, position_hardware_cursor, CursorPos, CURSOR_MARKER};

#[derive(Default)]
struct TestTerminal {
    output: String,
}

impl Terminal for TestTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.output.push_str(data);
    }
    fn columns(&self) -> u16 {
        80
    }
    fn rows(&self) -> u16 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: i32) {}
    fn hide_cursor(&mut self) {
        self.output.push_str("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.output.push_str("\x1b[?25h");
    }
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
}

#[test]
fn cursor_marker_and_hardware_cursor_match_fixture() {
    let expected = fixture::read_unescaped("cursor_output.txt");
    let mut lines = vec!["hello".to_string(), format!("wor{CURSOR_MARKER}ld")];
    let pos = extract_cursor_position(&mut lines, 2);
    assert_eq!(pos, Some(CursorPos { row: 1, col: 3 }));
    assert_eq!(lines[1], "world");

    let mut term = TestTerminal::default();
    let new_row = position_hardware_cursor(&mut term, pos, lines.len(), 0, true);
    assert_eq!(new_row, 1);
    assert_eq!(term.output, expected);
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
