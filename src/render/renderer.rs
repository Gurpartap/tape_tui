//! Diff renderer (Phase 4).

use crate::core::output::TerminalCmd;
use crate::core::text::slice::slice_by_column;
use crate::core::text::width::visible_width;
use crate::logging::{
    debug_redraw_enabled, log_debug_redraw, log_tui_debug, tui_debug_enabled, RenderDebugInfo,
};
use crate::render::Frame;

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
    force_full_redraw_next: bool,
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

    pub fn request_full_redraw_next(&mut self) {
        self.force_full_redraw_next = true;
    }

    pub fn previous_lines_len(&self) -> usize {
        self.previous_lines.len()
    }

    pub fn max_lines_rendered(&self) -> usize {
        self.max_lines_rendered
    }

    fn full_render(
        &mut self,
        lines: &[String],
        width: usize,
        height: usize,
        clear: bool,
    ) -> String {
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

        buffer
    }

    pub fn render(
        &mut self,
        frame: Frame,
        width: usize,
        height: usize,
        clear_on_shrink: bool,
        has_overlays: bool,
    ) -> Vec<TerminalCmd> {
        let mut lines = Vec::new();
        let mut is_image = Vec::new();
        for line in frame.into_lines() {
            is_image.push(line.is_image());
            lines.push(line.into_string());
        }
        let mut cmds = Vec::new();

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

        apply_line_resets(&mut lines, &is_image);

        // Snapshot strict-width mode once per render call (avoids repeated env reads).
        let strict_width = strict_width_enabled();
        let force_full_redraw_next = std::mem::take(&mut self.force_full_redraw_next);

        let width_changed = self.previous_width != 0 && self.previous_width != width;

        if self.previous_lines.is_empty() && !width_changed {
            if debug_redraw_enabled() {
                log_debug_redraw(
                    "first render",
                    self.previous_lines.len(),
                    lines.len(),
                    height,
                );
            }
            let buffer = self.full_render(&lines, width, height, false);
            cmds.push(TerminalCmd::Bytes(buffer));
            return cmds;
        }

        if width_changed {
            if debug_redraw_enabled() {
                let reason = format!("width changed ({} -> {})", self.previous_width, width);
                log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
            }
            let buffer = self.full_render(&lines, width, height, true);
            cmds.push(TerminalCmd::Bytes(buffer));
            return cmds;
        }

        if clear_on_shrink && lines.len() < self.max_lines_rendered && !has_overlays {
            if debug_redraw_enabled() {
                let reason = format!(
                    "clearOnShrink (maxLinesRendered={})",
                    self.max_lines_rendered
                );
                log_debug_redraw(&reason, self.previous_lines.len(), lines.len(), height);
            }
            let buffer = self.full_render(&lines, width, height, true);
            cmds.push(TerminalCmd::Bytes(buffer));
            return cmds;
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
            if force_full_redraw_next {
                let mut buffer = String::from(SYNC_START);

                if lines.is_empty() {
                    buffer.push('\r');
                    buffer.push_str("\x1b[2K");
                } else {
                    let first_row = lines.len().saturating_sub(height);
                    let render_end = lines.len().saturating_sub(1);

                    let prev_viewport_bottom = prev_viewport_top + height.saturating_sub(1);
                    let move_target_row = first_row;

                    if move_target_row > prev_viewport_bottom {
                        let current_screen_row = hardware_cursor_row
                            .saturating_sub(prev_viewport_top)
                            .min(height.saturating_sub(1));
                        let move_to_bottom =
                            height.saturating_sub(1).saturating_sub(current_screen_row);
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

                    buffer.push('\r');

                    for i in first_row..=render_end {
                        if i > first_row {
                            buffer.push_str("\r\n");
                        }
                        buffer.push_str("\x1b[2K");
                        let line = &lines[i];
                        if is_image[i] {
                            buffer.push_str(line);
                            continue;
                        }

                        let line_width = visible_width(line);
                        if line_width > width {
                            if strict_width {
                                panic!(
                                    "Rendered line {} exceeds terminal width ({} > {}). PI_STRICT_WIDTH is set.",
                                    i, line_width, width
                                );
                            }
                            buffer.push_str(&clamp_non_image_line_to_width(line, width));
                        } else {
                            buffer.push_str(line);
                        }
                    }
                }

                buffer.push_str(SYNC_END);
                cmds.push(TerminalCmd::Bytes(buffer));

                self.cursor_row = lines.len().saturating_sub(1);
                self.hardware_cursor_row = lines.len().saturating_sub(1);
                self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
                self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
                self.previous_lines = lines;
                self.previous_width = width;

                return cmds;
            }
            self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
            return cmds;
        };
        let last_changed = last_changed.unwrap_or(first_changed);

        let append_start =
            appended_lines && first_changed == self.previous_lines.len() && first_changed > 0;

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
                    let buffer = self.full_render(&lines, width, height, true);
                    cmds.push(TerminalCmd::Bytes(buffer));
                    return cmds;
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
                cmds.push(TerminalCmd::Bytes(buffer));
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.previous_lines = lines;
            self.previous_width = width;
            self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
            return cmds;
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
            let buffer = self.full_render(&lines, width, height, true);
            cmds.push(TerminalCmd::Bytes(buffer));
            return cmds;
        }

        let mut buffer = String::from(SYNC_START);
        let prev_viewport_bottom = prev_viewport_top + height.saturating_sub(1);
        let move_target_row = if append_start {
            first_changed.saturating_sub(1)
        } else {
            first_changed
        };

        if move_target_row > prev_viewport_bottom {
            let current_screen_row = hardware_cursor_row
                .saturating_sub(prev_viewport_top)
                .min(height.saturating_sub(1));
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
            if is_image[i] {
                buffer.push_str(line);
                continue;
            }

            let line_width = visible_width(line);
            if line_width > width {
                if strict_width {
                    panic!(
                        "Rendered line {} exceeds terminal width ({} > {}). PI_STRICT_WIDTH is set.",
                        i, line_width, width
                    );
                }
                buffer.push_str(&clamp_non_image_line_to_width(line, width));
            } else {
                buffer.push_str(line);
            }
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
        cmds.push(TerminalCmd::Bytes(buffer));

        self.cursor_row = lines.len().saturating_sub(1);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
        self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
        self.previous_lines = lines;
        self.previous_width = width;

        cmds
    }
}

fn apply_line_resets(lines: &mut [String], is_image: &[bool]) {
    debug_assert_eq!(lines.len(), is_image.len());
    for (line, is_image) in lines.iter_mut().zip(is_image.iter().copied()) {
        if !is_image {
            line.push_str(SEGMENT_RESET);
        }
    }
}

fn strict_width_enabled() -> bool {
    // Strict width checking is enabled when PI_STRICT_WIDTH is set to a non-empty value.
    std::env::var_os("PI_STRICT_WIDTH").is_some_and(|val| !val.is_empty())
}

fn clamp_non_image_line_to_width(line: &str, width: usize) -> String {
    // `apply_line_resets(..)` appends SEGMENT_RESET to all non-image lines prior to
    // diff rendering. Avoid duplicating those bytes when clamping.
    let line_without_reset = line.strip_suffix(SEGMENT_RESET).unwrap_or(line);

    let mut clamped = slice_by_column(line_without_reset, 0, width, true);
    clamped.push_str(SEGMENT_RESET);
    clamped
}

#[cfg(test)]
mod tests {
    use super::DiffRenderer;
    use crate::core::output::TerminalCmd;
    use crate::render::{Frame, Line, Span};
    use std::ffi::OsString;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct StrictWidthEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<OsString>,
    }

    impl StrictWidthEnvGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_LOCK.lock().expect("env lock poisoned");
            let prev = std::env::var_os("PI_STRICT_WIDTH");
            std::env::set_var("PI_STRICT_WIDTH", value);
            Self { _lock: lock, prev }
        }

        fn unset() -> Self {
            let lock = ENV_LOCK.lock().expect("env lock poisoned");
            let prev = std::env::var_os("PI_STRICT_WIDTH");
            std::env::remove_var("PI_STRICT_WIDTH");
            Self { _lock: lock, prev }
        }
    }

    impl Drop for StrictWidthEnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(val) => std::env::set_var("PI_STRICT_WIDTH", val),
                None => std::env::remove_var("PI_STRICT_WIDTH"),
            }
        }
    }

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
    fn width_change_triggers_full_clear() {
        let mut renderer = DiffRenderer::new();
        let output =
            cmds_to_bytes(renderer.render(vec!["line".to_string()].into(), 10, 5, false, false));
        assert!(!output.is_empty());

        let output =
            cmds_to_bytes(renderer.render(vec!["line".to_string()].into(), 12, 5, false, false));
        assert!(output.contains("\x1b[3J\x1b[2J\x1b[H"));
    }

    #[test]
    fn diff_renders_only_changed_lines() {
        let mut renderer = DiffRenderer::new();
        renderer.render(
            vec!["one".to_string(), "two".to_string()].into(),
            20,
            5,
            false,
            false,
        );

        let output = cmds_to_bytes(renderer.render(
            vec!["one".to_string(), "tWO".to_string()].into(),
            20,
            5,
            false,
            false,
        ));
        assert!(output.contains("tWO"));
        assert!(!output.contains("one"));
    }

    #[test]
    fn overflow_clamps_on_diff_path_by_default() {
        let _guard = StrictWidthEnvGuard::unset();
        let mut renderer = DiffRenderer::new();
        renderer.render(vec!["123456".to_string()].into(), 5, 5, false, false);

        let output =
            cmds_to_bytes(renderer.render(vec!["abcdef".to_string()].into(), 5, 5, false, false));
        assert!(output.contains("abcde\x1b[0m\x1b]8;;\x07"));
        assert!(!output.contains("abcdef"));
    }

    #[test]
    fn overflow_panics_on_diff_path_in_strict_mode() {
        let _guard = StrictWidthEnvGuard::set("1");
        let mut renderer = DiffRenderer::new();
        renderer.render(vec!["123456".to_string()].into(), 5, 5, false, false);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.render(vec!["abcdef".to_string()].into(), 5, 5, false, false);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn identical_render_produces_no_output() {
        let mut renderer = DiffRenderer::new();
        renderer.render(vec!["line".to_string()].into(), 20, 5, false, false);

        let output =
            cmds_to_bytes(renderer.render(vec!["line".to_string()].into(), 20, 5, false, false));
        assert!(output.is_empty(), "expected no output, got: {output:?}");
    }

    #[test]
    fn force_full_redraw_emits_output_even_if_identical() {
        let mut renderer = DiffRenderer::new();
        let render = |r: &mut DiffRenderer| {
            r.render(
                vec!["one".to_string(), "two".to_string()].into(),
                20,
                5,
                false,
                false,
            )
        };

        assert!(!render(&mut renderer).is_empty(), "expected first render output");

        let cmds = render(&mut renderer);
        assert!(
            cmds.is_empty(),
            "expected identical render to produce no cmds, got: {cmds:?}"
        );

        renderer.request_full_redraw_next();
        let forced = cmds_to_bytes(render(&mut renderer));
        assert!(forced.contains(super::SYNC_START));
        assert!(forced.contains(super::SYNC_END));
        assert_eq!(forced.matches("\x1b[2K").count(), 2);
        assert!(!forced.contains(super::CLEAR_ALL));
        assert!(!forced.contains("\x1b[3J"));

        let cmds = render(&mut renderer);
        assert!(
            cmds.is_empty(),
            "expected request flag to be consumed, got: {cmds:?}"
        );
    }

    #[test]
    fn segment_reset_appended_to_non_image_lines() {
        let mut renderer = DiffRenderer::new();
        let output =
            cmds_to_bytes(renderer.render(vec!["hello".to_string()].into(), 20, 5, false, false));
        assert!(output.contains("hello\x1b[0m\x1b]8;;\x07"));
    }

    #[test]
    fn multi_span_line_renders_identically_to_concatenated_line() {
        let mut renderer_multi = DiffRenderer::new();
        let multi = Frame::new(vec![Line::new(vec![
            Span::new("he".to_string()),
            Span::new("llo".to_string()),
        ])]);
        let multi_out = cmds_to_bytes(renderer_multi.render(multi, 20, 5, false, false));

        let mut renderer_single = DiffRenderer::new();
        let single: Frame = vec!["hello".to_string()].into();
        let single_out = cmds_to_bytes(renderer_single.render(single, 20, 5, false, false));

        assert_eq!(multi_out, single_out);
    }

    #[test]
    fn typed_image_lines_bypass_width_check_in_strict_mode() {
        let _guard = StrictWidthEnvGuard::set("1");
        let mut renderer = DiffRenderer::new();
        renderer.render(vec!["short".to_string()].into(), 5, 5, false, false);

        let image_frame = Frame::new(vec![Line::image(vec![Span::new("X".repeat(100))])]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.render(image_frame, 5, 5, false, false);
        }));
        assert!(result.is_ok());
    }
}
