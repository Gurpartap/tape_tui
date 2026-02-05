//! Diff renderer (Phase 4).

use crate::core::terminal::Terminal;
use crate::logging::{
    debug_redraw_enabled, log_debug_redraw, log_tui_debug, tui_debug_enabled, RenderDebugInfo,
};
use crate::render::width::visible_width;

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";
const SYNC_START: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const CLEAR_ALL: &str = "\x1b[3J\x1b[2J\x1b[H";

#[derive(Debug, Default)]
pub struct DiffRenderer {
    previous_lines: Vec<String>,
    previous_width: usize,
    max_lines_rendered: usize,
    cursor_row: usize,
    hardware_cursor_row: usize,
    previous_viewport_top: usize,
}

impl DiffRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hardware_cursor_row(&self) -> usize {
        self.hardware_cursor_row
    }

    pub fn set_hardware_cursor_row(&mut self, row: usize) {
        self.hardware_cursor_row = row;
    }

    pub fn previous_lines_len(&self) -> usize {
        self.previous_lines.len()
    }

    fn full_render(
        &mut self,
        term: &mut dyn Terminal,
        lines: &[String],
        width: usize,
        height: usize,
        clear: bool,
    ) {
        let mut buffer = String::from(SYNC_START);
        if clear {
            buffer.push_str(CLEAR_ALL);
        }
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                buffer.push_str("\r\n");
            }
            buffer.push_str(line);
        }
        buffer.push_str(SYNC_END);
        term.write(&buffer);

        self.cursor_row = lines.len().saturating_sub(1);
        self.hardware_cursor_row = self.cursor_row;
        if clear {
            self.max_lines_rendered = lines.len();
        } else {
            self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
        }
        self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
        self.previous_lines = lines.to_vec();
        self.previous_width = width;
    }

    pub fn render(
        &mut self,
        term: &mut dyn Terminal,
        mut lines: Vec<String>,
        is_image_line: fn(&str) -> bool,
        clear_on_shrink: bool,
        has_overlays: bool,
    ) {
        let width = term.columns() as usize;
        let height = term.rows() as usize;

        let mut viewport_top = self.max_lines_rendered.saturating_sub(height);
        let mut prev_viewport_top = self.previous_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let compute_line_diff = |target_row: usize,
                                 hardware_cursor_row: usize,
                                 prev_viewport_top: usize,
                                 viewport_top: usize|
         -> i32 {
            let current_screen_row = hardware_cursor_row.saturating_sub(prev_viewport_top) as i32;
            let target_screen_row = target_row.saturating_sub(viewport_top) as i32;
            target_screen_row - current_screen_row
        };

        apply_line_resets(&mut lines, is_image_line);

        let width_changed = self.previous_width != 0 && self.previous_width != width;

        if self.previous_lines.is_empty() && !width_changed {
            if debug_redraw_enabled() {
                log_debug_redraw("first render", self.previous_lines.len(), lines.len(), height);
            }
            self.full_render(term, &lines, width, height, false);
            return;
        }

        if width_changed {
            if debug_redraw_enabled() {
                let reason = format!("width changed ({} -> {})", self.previous_width, width);
                log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
            }
            self.full_render(term, &lines, width, height, true);
            return;
        }

        if clear_on_shrink && lines.len() < self.max_lines_rendered && !has_overlays {
            if debug_redraw_enabled() {
                let reason = format!("clearOnShrink (maxLinesRendered={})", self.max_lines_rendered);
                log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
            }
            self.full_render(term, &lines, width, height, true);
            return;
        }

        let mut first_changed: Option<usize> = None;
        let mut last_changed: Option<usize> = None;
        let max_lines = lines.len().max(self.previous_lines.len());
        for i in 0..max_lines {
            let old_line = self.previous_lines.get(i).map(String::as_str).unwrap_or("");
            let new_line = lines.get(i).map(String::as_str).unwrap_or("");
            if old_line != new_line {
                if first_changed.is_none() {
                    first_changed = Some(i);
                }
                last_changed = Some(i);
            }
        }

        let appended_lines = lines.len() > self.previous_lines.len();
        if appended_lines {
            if first_changed.is_none() {
                first_changed = Some(self.previous_lines.len());
            }
            last_changed = Some(lines.len().saturating_sub(1));
        }

        let Some(first_changed) = first_changed else {
            self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
            return;
        };
        let last_changed = last_changed.unwrap_or(first_changed);

        let append_start = appended_lines && first_changed == self.previous_lines.len() && first_changed > 0;

        if first_changed >= lines.len() {
            if self.previous_lines.len() > lines.len() {
                let mut buffer = String::from(SYNC_START);
                let target_row = lines.len().saturating_sub(1);
                let diff = compute_line_diff(
                    target_row,
                    hardware_cursor_row,
                    prev_viewport_top,
                    viewport_top,
                );
                if diff > 0 {
                    buffer.push_str(&format!("\x1b[{}B", diff));
                } else if diff < 0 {
                    buffer.push_str(&format!("\x1b[{}A", -diff));
                }
                buffer.push('\r');

                let extra_lines = self.previous_lines.len().saturating_sub(lines.len());
                if extra_lines > height {
                    if debug_redraw_enabled() {
                        let reason = format!("extraLines > height ({} > {})", extra_lines, height);
                        log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
                    }
                    self.full_render(term, &lines, width, height, true);
                    return;
                }
                if extra_lines > 0 {
                    buffer.push_str("\x1b[1B");
                }
                for i in 0..extra_lines {
                    buffer.push_str("\r\x1b[2K");
                    if i + 1 < extra_lines {
                        buffer.push_str("\x1b[1B");
                    }
                }
                if extra_lines > 0 {
                    buffer.push_str(&format!("\x1b[{}A", extra_lines));
                }
                buffer.push_str(SYNC_END);
                term.write(&buffer);
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.previous_lines = lines;
            self.previous_width = width;
            self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
            return;
        }

        let previous_content_viewport_top = self.previous_lines.len().saturating_sub(height);
        if first_changed < previous_content_viewport_top {
            if debug_redraw_enabled() {
                let reason = format!(
                    "firstChanged < viewportTop ({} < {})",
                    first_changed, previous_content_viewport_top
                );
                log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
            }
            self.full_render(term, &lines, width, height, true);
            return;
        }

        let mut buffer = String::from(SYNC_START);
        let prev_viewport_bottom = prev_viewport_top + height.saturating_sub(1);
        let move_target_row = if append_start {
            first_changed.saturating_sub(1)
        } else {
            first_changed
        };

        if move_target_row > prev_viewport_bottom {
            let current_screen_row =
                hardware_cursor_row.saturating_sub(prev_viewport_top).min(height.saturating_sub(1));
            let move_to_bottom = height.saturating_sub(1).saturating_sub(current_screen_row);
            if move_to_bottom > 0 {
                buffer.push_str(&format!("\x1b[{}B", move_to_bottom));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            for _ in 0..scroll {
                buffer.push_str("\r\n");
            }
            prev_viewport_top = prev_viewport_top.saturating_add(scroll);
            viewport_top = viewport_top.saturating_add(scroll);
            hardware_cursor_row = move_target_row;
        }

        let line_diff = compute_line_diff(
            move_target_row,
            hardware_cursor_row,
            prev_viewport_top,
            viewport_top,
        );
        if line_diff > 0 {
            buffer.push_str(&format!("\x1b[{}B", line_diff));
        } else if line_diff < 0 {
            buffer.push_str(&format!("\x1b[{}A", -line_diff));
        }

        if append_start {
            buffer.push_str("\r\n");
        } else {
            buffer.push('\r');
        }

        let render_end = last_changed.min(lines.len().saturating_sub(1));
        for i in first_changed..=render_end {
            if i > first_changed {
                buffer.push_str("\r\n");
            }
            buffer.push_str("\x1b[2K");
            let line = &lines[i];
            if !is_image_line(line) && visible_width(line) > width {
                panic!(
                    "Rendered line {} exceeds terminal width ({} > {}).",
                    i,
                    visible_width(line),
                    width
                );
            }
            buffer.push_str(line);
        }

        let mut final_cursor_row = render_end;

        if self.previous_lines.len() > lines.len() {
            if render_end < lines.len().saturating_sub(1) {
                let move_down = lines.len().saturating_sub(1).saturating_sub(render_end);
                buffer.push_str(&format!("\x1b[{}B", move_down));
                final_cursor_row = lines.len().saturating_sub(1);
            }
            let extra_lines = self.previous_lines.len().saturating_sub(lines.len());
            for _ in 0..extra_lines {
                buffer.push_str("\r\n\x1b[2K");
            }
            if extra_lines > 0 {
                buffer.push_str(&format!("\x1b[{}A", extra_lines));
            }
        }

        buffer.push_str(SYNC_END);
        if tui_debug_enabled() {
            let info = RenderDebugInfo {
                first_changed,
                viewport_top,
                cursor_row: self.cursor_row,
                height,
                line_diff,
                hardware_cursor_row,
                render_end,
                final_cursor_row,
                new_lines: &lines,
                previous_lines: &self.previous_lines,
                buffer: &buffer,
            };
            log_tui_debug(&info);
        }
        term.write(&buffer);

        self.cursor_row = lines.len().saturating_sub(1);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
        self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
        self.previous_lines = lines;
        self.previous_width = width;
    }
}

fn apply_line_resets(lines: &mut [String], is_image_line: fn(&str) -> bool) {
    for line in lines.iter_mut() {
        if !is_image_line(line) {
            line.push_str(SEGMENT_RESET);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiffRenderer;
    use crate::core::terminal::Terminal;

    #[derive(Default)]
    struct TestTerminal {
        output: String,
        columns: u16,
        rows: u16,
    }

    impl TestTerminal {
        fn new(columns: u16, rows: u16) -> Self {
            Self {
                output: String::new(),
                columns,
                rows,
            }
        }

        fn take_output(&mut self) -> String {
            std::mem::take(&mut self.output)
        }
    }

    impl Terminal for TestTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
        fn stop(&mut self) {}
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            self.columns
        }
        fn rows(&self) -> u16 {
            self.rows
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: i32) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
    }

    fn not_image(_: &str) -> bool {
        false
    }

    #[test]
    fn width_change_triggers_full_clear() {
        let mut renderer = DiffRenderer::new();
        let mut term = TestTerminal::new(10, 5);
        renderer.render(&mut term, vec!["line".to_string()], not_image, false, false);
        term.take_output();

        term.columns = 12;
        renderer.render(&mut term, vec!["line".to_string()], not_image, false, false);
        let output = term.take_output();
        assert!(output.contains("\x1b[3J\x1b[2J\x1b[H"));
    }

    #[test]
    fn diff_renders_only_changed_lines() {
        let mut renderer = DiffRenderer::new();
        let mut term = TestTerminal::new(20, 5);
        renderer.render(
            &mut term,
            vec!["one".to_string(), "two".to_string()],
            not_image,
            false,
            false,
        );
        term.take_output();

        renderer.render(
            &mut term,
            vec!["one".to_string(), "tWO".to_string()],
            not_image,
            false,
            false,
        );
        let output = term.take_output();
        assert!(output.contains("tWO"));
        assert!(!output.contains("one"));
    }

    #[test]
    fn overflow_panics_only_on_diff_path() {
        let mut renderer = DiffRenderer::new();
        let mut term = TestTerminal::new(5, 5);
        renderer.render(&mut term, vec!["123456".to_string()], not_image, false, false);
        term.take_output();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.render(&mut term, vec!["abcdef".to_string()], not_image, false, false);
        }));
        assert!(result.is_err());
    }
}
