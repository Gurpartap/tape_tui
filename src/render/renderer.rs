//! Diff renderer.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InsertBeforeFastPathPlan {
    insertion_at: usize,
    inserted_count: usize,
    previous_viewport_top: usize,
    new_viewport_top: usize,
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

    /// Resets internal bookkeeping after the runtime clears the visible screen out-of-band
    /// (e.g. `ESC[2J ESC[H`).
    ///
    /// This is intentionally *not* a `CLEAR_ALL` (`ESC[3J`) reset: the next `render(..)` call
    /// should behave like a first render and re-emit the full frame without clearing scrollback.
    pub fn reset_for_external_clear_screen(&mut self) {
        self.previous_lines.clear();
        self.previous_width = 0;
        self.max_lines_rendered = 0;
        self.cursor_row = 0;
        self.hardware_cursor_row = 0;
        self.previous_viewport_top = 0;
        self.force_full_redraw_next = false;
    }

    /// Applies a cursor move that happened out-of-band (only `CSI nA` / `CSI nB`, no scrolling).
    ///
    /// The terminal clamps cursor movement to the visible viewport. We mirror that clamp in the
    /// renderer's stored `hardware_cursor_row` (absolute row coordinates).
    ///
    /// Note: This does not mutate `previous_lines` because it is strictly cursor bookkeeping.
    pub fn apply_out_of_band_move_by(&mut self, delta: i32, term_height: usize) {
        let viewport_top = self.max_lines_rendered.saturating_sub(term_height);
        let viewport_bottom = viewport_top + term_height.saturating_sub(1);

        let next = self.hardware_cursor_row as i64 + delta as i64;
        let clamped = next
            .clamp(viewport_top as i64, viewport_bottom as i64)
            .max(0) as usize;

        self.hardware_cursor_row = clamped;
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
        has_surfaces: bool,
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
        let insert_before_fast_path_plan = compute_insert_before_fast_path_eligibility(
            &self.previous_lines,
            &lines,
            &is_image,
            width,
            self.previous_width,
            height,
            self.previous_viewport_top,
            self.max_lines_rendered,
            self.hardware_cursor_row,
            has_surfaces,
        );

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

        if clear_on_shrink && lines.len() < self.max_lines_rendered && !has_surfaces {
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
                                    "Rendered line {} exceeds terminal width ({} > {}). TAPE_STRICT_WIDTH is set.",
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

        if let Some(plan) = insert_before_fast_path_plan {
            if first_changed == plan.insertion_at {
                let (buffer, final_cursor_row) = emit_insert_before_fast_path(
                    &lines,
                    plan,
                    width,
                    height,
                    strict_width,
                    hardware_cursor_row,
                );
                cmds.push(TerminalCmd::Bytes(buffer));

                self.cursor_row = lines.len().saturating_sub(1);
                self.hardware_cursor_row = final_cursor_row;
                self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
                self.previous_viewport_top = self.max_lines_rendered.saturating_sub(height);
                self.previous_lines = lines;
                self.previous_width = width;

                return cmds;
            }
        }

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
                        "Rendered line {} exceeds terminal width ({} > {}). TAPE_STRICT_WIDTH is set.",
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

fn compute_insert_before_fast_path_eligibility(
    previous_lines: &[String],
    new_lines: &[String],
    new_line_is_image: &[bool],
    width: usize,
    previous_width: usize,
    height: usize,
    previous_viewport_top: usize,
    previous_max_lines_rendered: usize,
    hardware_cursor_row: usize,
    has_surfaces: bool,
) -> Option<InsertBeforeFastPathPlan> {
    if previous_lines.is_empty() || new_lines.len() <= previous_lines.len() {
        return None;
    }
    if width == 0 || height == 0 || previous_width == 0 || previous_width != width {
        return None;
    }
    if has_surfaces {
        return None;
    }

    let inserted_count = new_lines.len().saturating_sub(previous_lines.len());
    if inserted_count == 0 || inserted_count > height {
        return None;
    }

    if previous_max_lines_rendered != previous_lines.len() {
        return None;
    }

    let previous_content_viewport_top = previous_lines.len().saturating_sub(height);
    if previous_viewport_top != previous_content_viewport_top || previous_viewport_top == 0 {
        return None;
    }

    if new_line_is_image.iter().copied().any(|is_image| is_image)
        || previous_lines
            .iter()
            .any(|line| crate::core::terminal_image::is_image_line(line))
    {
        return None;
    }

    let previous_viewport_bottom = previous_viewport_top + height.saturating_sub(1);
    if hardware_cursor_row < previous_viewport_top || hardware_cursor_row > previous_viewport_bottom
    {
        return None;
    }

    let insertion_at = (0..previous_lines.len())
        .find(|idx| previous_lines[*idx] != new_lines[*idx])
        .unwrap_or(previous_lines.len());

    if insertion_at >= previous_viewport_top {
        return None;
    }

    for idx in insertion_at..previous_lines.len() {
        let shifted_idx = idx + inserted_count;
        if new_lines.get(shifted_idx) != previous_lines.get(idx) {
            return None;
        }
    }

    let new_viewport_top = new_lines.len().saturating_sub(height);
    if new_viewport_top != previous_viewport_top.saturating_add(inserted_count) {
        return None;
    }

    Some(InsertBeforeFastPathPlan {
        insertion_at,
        inserted_count,
        previous_viewport_top,
        new_viewport_top,
    })
}

fn append_non_image_line_with_width_guard(
    buffer: &mut String,
    line: &str,
    width: usize,
    strict_width: bool,
    line_index: usize,
) {
    let line_width = visible_width(line);
    if line_width > width {
        if strict_width {
            panic!(
                "Rendered line {} exceeds terminal width ({} > {}). TAPE_STRICT_WIDTH is set.",
                line_index, line_width, width
            );
        }
        buffer.push_str(&clamp_non_image_line_to_width(line, width));
    } else {
        buffer.push_str(line);
    }
}

fn emit_insert_before_fast_path(
    lines: &[String],
    plan: InsertBeforeFastPathPlan,
    width: usize,
    height: usize,
    strict_width: bool,
    mut hardware_cursor_row: usize,
) -> (String, usize) {
    let mut buffer = String::from(SYNC_START);

    let previous_viewport_bottom = plan.previous_viewport_top + height.saturating_sub(1);
    let current_screen_row = hardware_cursor_row.saturating_sub(plan.previous_viewport_top) as i32;
    let target_screen_row =
        previous_viewport_bottom.saturating_sub(plan.previous_viewport_top) as i32;
    let line_diff = target_screen_row - current_screen_row;
    if line_diff > 0 {
        buffer.push_str(&format!("\x1b[{}B", line_diff));
    } else if line_diff < 0 {
        buffer.push_str(&format!("\x1b[{}A", -line_diff));
    }
    hardware_cursor_row = previous_viewport_bottom;

    for row in plan.insertion_at..plan.insertion_at + plan.inserted_count {
        buffer.push('\r');
        buffer.push_str("\x1b[2K");
        append_non_image_line_with_width_guard(&mut buffer, &lines[row], width, strict_width, row);
        buffer.push_str("\r\n");
        hardware_cursor_row = hardware_cursor_row.saturating_add(1);
    }

    let move_up = height.saturating_sub(1);
    if move_up > 0 {
        buffer.push_str(&format!("\x1b[{}A", move_up));
        hardware_cursor_row = hardware_cursor_row.saturating_sub(move_up);
    }

    for (offset, row) in (plan.new_viewport_top..lines.len()).enumerate() {
        if offset > 0 {
            buffer.push_str("\r\n");
            hardware_cursor_row = hardware_cursor_row.saturating_add(1);
        }
        buffer.push('\r');
        buffer.push_str("\x1b[2K");
        append_non_image_line_with_width_guard(&mut buffer, &lines[row], width, strict_width, row);
    }

    buffer.push_str(SYNC_END);
    (buffer, hardware_cursor_row)
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
    // Strict width checking is enabled when TAPE_STRICT_WIDTH is set to a non-empty value.
    std::env::var_os("TAPE_STRICT_WIDTH").is_some_and(|val| !val.is_empty())
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
            let prev = std::env::var_os("TAPE_STRICT_WIDTH");
            std::env::set_var("TAPE_STRICT_WIDTH", value);
            Self { _lock: lock, prev }
        }

        fn unset() -> Self {
            let lock = ENV_LOCK.lock().expect("env lock poisoned");
            let prev = std::env::var_os("TAPE_STRICT_WIDTH");
            std::env::remove_var("TAPE_STRICT_WIDTH");
            Self { _lock: lock, prev }
        }
    }

    impl Drop for StrictWidthEnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(val) => std::env::set_var("TAPE_STRICT_WIDTH", val),
                None => std::env::remove_var("TAPE_STRICT_WIDTH"),
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

    fn non_image_lines(lines: &[&str]) -> Vec<String> {
        let mut rendered = lines
            .iter()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>();
        let is_image = vec![false; rendered.len()];
        super::apply_line_resets(&mut rendered, &is_image);
        rendered
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct VisibleSnapshot {
        rows: Vec<String>,
        cursor_row: usize,
        cursor_col: usize,
    }

    fn parse_csi_first_param(params: &str, default: usize) -> usize {
        params
            .trim_start_matches('?')
            .split(';')
            .next()
            .and_then(|value| {
                if value.is_empty() {
                    None
                } else {
                    value.parse::<usize>().ok()
                }
            })
            .unwrap_or(default)
    }

    fn simulate_visible_snapshot(bytes: &str, width: usize, height: usize) -> VisibleSnapshot {
        fn clear_line(screen: &mut [Vec<char>], row: usize) {
            if row >= screen.len() {
                return;
            }
            for cell in screen[row].iter_mut() {
                *cell = ' ';
            }
        }

        fn clear_screen(screen: &mut [Vec<char>]) {
            for row in 0..screen.len() {
                clear_line(screen, row);
            }
        }

        fn scroll_up(screen: &mut Vec<Vec<char>>, width: usize) {
            screen.remove(0);
            screen.push(vec![' '; width]);
        }

        assert!(width > 0);
        assert!(height > 0);

        let mut screen = vec![vec![' '; width]; height];
        let mut row = 0usize;
        let mut col = 0usize;

        let bytes = bytes.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            match bytes[index] {
                b'\x1b' => {
                    if index + 1 >= bytes.len() {
                        break;
                    }

                    match bytes[index + 1] {
                        b'[' => {
                            let mut end = index + 2;
                            while end < bytes.len() && !(0x40..=0x7E).contains(&bytes[end]) {
                                end += 1;
                            }
                            if end >= bytes.len() {
                                break;
                            }

                            let params = std::str::from_utf8(&bytes[index + 2..end])
                                .expect("CSI params must be utf-8 digits");
                            let final_byte = bytes[end] as char;
                            match final_byte {
                                'A' => {
                                    let delta = parse_csi_first_param(params, 1);
                                    row = row.saturating_sub(delta);
                                }
                                'B' => {
                                    let delta = parse_csi_first_param(params, 1);
                                    row = row.saturating_add(delta).min(height.saturating_sub(1));
                                }
                                'G' => {
                                    let target_col = parse_csi_first_param(params, 1);
                                    col = target_col.saturating_sub(1).min(width.saturating_sub(1));
                                }
                                'H' => {
                                    row = 0;
                                    col = 0;
                                }
                                'J' => {
                                    let mode = parse_csi_first_param(params, 0);
                                    if mode == 2 {
                                        clear_screen(&mut screen);
                                        row = 0;
                                        col = 0;
                                    }
                                }
                                'K' => {
                                    let mode = parse_csi_first_param(params, 0);
                                    if mode == 2 {
                                        clear_line(&mut screen, row);
                                    }
                                }
                                'm' | 'h' | 'l' => {
                                    // Styling and DEC private toggles do not affect visible cells.
                                }
                                _ => {}
                            }

                            index = end + 1;
                            continue;
                        }
                        b']' => {
                            let mut end = index + 2;
                            while end < bytes.len() {
                                if bytes[end] == 0x07 {
                                    end += 1;
                                    break;
                                }
                                if end + 1 < bytes.len()
                                    && bytes[end] == b'\x1b'
                                    && bytes[end + 1] == b'\\'
                                {
                                    end += 2;
                                    break;
                                }
                                end += 1;
                            }
                            index = end;
                            continue;
                        }
                        _ => {
                            index += 1;
                            continue;
                        }
                    }
                }
                b'\r' => {
                    col = 0;
                    index += 1;
                    continue;
                }
                b'\n' => {
                    if row + 1 >= height {
                        scroll_up(&mut screen, width);
                        row = height.saturating_sub(1);
                    } else {
                        row += 1;
                    }
                    index += 1;
                    continue;
                }
                byte => {
                    if row < height && col < width {
                        screen[row][col] = byte as char;
                    }
                    col = col.saturating_add(1).min(width.saturating_sub(1));
                    index += 1;
                    continue;
                }
            }
        }

        let rows = screen
            .into_iter()
            .map(|row| {
                let mut line: String = row.into_iter().collect();
                while line.ends_with(' ') {
                    line.pop();
                }
                line
            })
            .collect();

        VisibleSnapshot {
            rows,
            cursor_row: row,
            cursor_col: col,
        }
    }

    #[test]
    fn eligibility_accepts_pure_insert_before_previous_viewport() {
        let previous_lines =
            non_image_lines(&["line-0", "line-1", "line-2", "line-3", "line-4", "line-5"]);
        let new_lines = non_image_lines(&[
            "history-a",
            "history-b",
            "line-0",
            "line-1",
            "line-2",
            "line-3",
            "line-4",
            "line-5",
        ]);
        let new_line_is_image = vec![false; new_lines.len()];

        let plan = super::compute_insert_before_fast_path_eligibility(
            &previous_lines,
            &new_lines,
            &new_line_is_image,
            20,
            20,
            3,
            3,
            previous_lines.len(),
            5,
            false,
        )
        .expect("expected eligibility to activate");

        assert_eq!(plan.insertion_at, 0);
        assert_eq!(plan.inserted_count, 2);
        assert_eq!(plan.previous_viewport_top, 3);
        assert_eq!(plan.new_viewport_top, 5);
    }

    #[test]
    fn eligibility_rejects_when_state_constraints_are_not_safe() {
        let previous_lines =
            non_image_lines(&["line-0", "line-1", "line-2", "line-3", "line-4", "line-5"]);
        let new_lines = non_image_lines(&[
            "history-a",
            "history-b",
            "line-0",
            "line-1",
            "line-2",
            "line-3",
            "line-4",
            "line-5",
        ]);
        let new_line_is_image = vec![false; new_lines.len()];

        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &new_lines,
                &new_line_is_image,
                20,
                19,
                3,
                3,
                previous_lines.len(),
                5,
                false,
            )
            .is_none(),
            "width changes must force fallback"
        );

        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &new_lines,
                &new_line_is_image,
                20,
                20,
                3,
                3,
                previous_lines.len(),
                1,
                false,
            )
            .is_none(),
            "cursor rows outside prior viewport must force fallback"
        );

        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &new_lines,
                &new_line_is_image,
                20,
                20,
                3,
                3,
                previous_lines.len().saturating_sub(1),
                5,
                false,
            )
            .is_none(),
            "stale max-lines bookkeeping must force fallback"
        );

        let insertion_touches_viewport = non_image_lines(&[
            "line-0",
            "line-1",
            "line-2",
            "history-a",
            "line-3",
            "line-4",
            "line-5",
        ]);
        let insertion_touches_viewport_is_image = vec![false; insertion_touches_viewport.len()];
        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &insertion_touches_viewport,
                &insertion_touches_viewport_is_image,
                20,
                20,
                3,
                3,
                previous_lines.len(),
                5,
                false,
            )
            .is_none(),
            "insertions at or below the previous viewport top must force fallback"
        );

        let mut not_pure_insertion = new_lines.clone();
        not_pure_insertion[4] = format!("line-2-mutated{}", super::SEGMENT_RESET);
        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &not_pure_insertion,
                &new_line_is_image,
                20,
                20,
                3,
                3,
                previous_lines.len(),
                5,
                false,
            )
            .is_none(),
            "non-insertion diffs must force fallback"
        );

        let mut new_line_is_image = vec![false; new_lines.len()];
        new_line_is_image[0] = true;
        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &new_lines,
                &new_line_is_image,
                20,
                20,
                3,
                3,
                previous_lines.len(),
                5,
                false,
            )
            .is_none(),
            "image lines must force fallback"
        );

        assert!(
            super::compute_insert_before_fast_path_eligibility(
                &previous_lines,
                &new_lines,
                &vec![false; new_lines.len()],
                20,
                20,
                3,
                3,
                previous_lines.len(),
                5,
                true,
            )
            .is_none(),
            "surface compositing must force fallback"
        );
    }

    #[test]
    fn fast_path_emits_insert_before_sequence_without_full_clear() {
        let mut renderer = DiffRenderer::new();
        let height = 3;

        let base_lines: Vec<String> = (0..6).map(|i| format!("line-{i}")).collect();
        renderer.render(base_lines.clone().into(), 20, height, false, false);

        let mut grown_lines = vec!["history-a".to_string(), "history-b".to_string()];
        grown_lines.extend(base_lines);
        let output = cmds_to_bytes(renderer.render(grown_lines.into(), 20, height, false, false));

        let expected = format!(
            "{}\r\x1b[2Khistory-a{}\r\n\r\x1b[2Khistory-b{}\r\n\x1b[2A\r\x1b[2Kline-3{}\r\n\r\x1b[2Kline-4{}\r\n\r\x1b[2Kline-5{}{}",
            super::SYNC_START,
            super::SEGMENT_RESET,
            super::SEGMENT_RESET,
            super::SEGMENT_RESET,
            super::SEGMENT_RESET,
            super::SEGMENT_RESET,
            super::SYNC_END,
        );

        assert_eq!(output, expected);
        assert!(!output.contains(super::CLEAR_ALL));
        assert_eq!(renderer.previous_lines_len(), 8);
        assert_eq!(renderer.max_lines_rendered(), 8);
        assert_eq!(renderer.hardware_cursor_row(), 7);
    }

    #[test]
    fn fast_path_falls_back_to_full_redraw_when_cursor_state_is_unsafe() {
        let mut renderer = DiffRenderer::new();
        let height = 3;

        let base_lines: Vec<String> = (0..6).map(|i| format!("line-{i}")).collect();
        renderer.render(base_lines.clone().into(), 20, height, false, false);

        renderer.set_hardware_cursor_row(0);

        let mut grown_lines = vec!["history-a".to_string(), "history-b".to_string()];
        grown_lines.extend(base_lines);
        let output = cmds_to_bytes(renderer.render(grown_lines.into(), 20, height, false, false));

        assert!(
            output.contains(super::CLEAR_ALL),
            "unsafe cursor state should force fallback full redraw, got: {output:?}"
        );
    }

    #[test]
    fn fast_and_fallback_paths_are_visually_equivalent() {
        let width = 20;
        let height = 3;

        let base_lines: Vec<String> = (0..6).map(|i| format!("line-{i}")).collect();
        let mut grown_lines = vec!["history-a".to_string(), "history-b".to_string()];
        grown_lines.extend(base_lines.clone());

        let mut fast_renderer = DiffRenderer::new();
        fast_renderer.render(base_lines.clone().into(), width, height, false, false);
        let fast_output = cmds_to_bytes(fast_renderer.render(
            grown_lines.clone().into(),
            width,
            height,
            false,
            false,
        ));

        let mut fallback_renderer = DiffRenderer::new();
        fallback_renderer.render(base_lines.into(), width, height, false, false);
        let fallback_output =
            cmds_to_bytes(fallback_renderer.render(grown_lines.into(), width, height, false, true));

        assert!(
            !fast_output.contains(super::CLEAR_ALL),
            "expected optimized path to avoid full clear, got: {fast_output:?}"
        );
        assert!(
            fallback_output.contains(super::CLEAR_ALL),
            "expected forced fallback to use full clear path, got: {fallback_output:?}"
        );

        let fast_visible = simulate_visible_snapshot(&fast_output, width, height);
        let fallback_visible = simulate_visible_snapshot(&fallback_output, width, height);
        assert_eq!(fast_visible, fallback_visible);

        assert_eq!(
            fast_renderer.hardware_cursor_row(),
            fallback_renderer.hardware_cursor_row(),
            "hardware cursor row must stay deterministic across path selection"
        );
    }

    #[test]
    fn ineligible_surface_or_image_conditions_force_baseline_fallback() {
        let width = 20;
        let height = 3;
        let base_lines: Vec<String> = (0..6).map(|i| format!("line-{i}")).collect();

        let mut with_surface = DiffRenderer::new();
        with_surface.render(base_lines.clone().into(), width, height, false, false);
        let mut grown_lines = vec!["history-a".to_string(), "history-b".to_string()];
        grown_lines.extend(base_lines.clone());
        let with_surface_output =
            cmds_to_bytes(with_surface.render(grown_lines.into(), width, height, false, true));
        assert!(
            with_surface_output.contains(super::CLEAR_ALL),
            "surface composition must force baseline fallback, got: {with_surface_output:?}"
        );

        let mut with_image = DiffRenderer::new();
        with_image.render(base_lines.clone().into(), width, height, false, false);
        let mut with_image_growth = vec!["\x1b_Gimage-inline".to_string(), "history-b".to_string()];
        with_image_growth.extend(base_lines);
        let with_image_output =
            cmds_to_bytes(with_image.render(with_image_growth.into(), width, height, false, false));
        assert!(
            with_image_output.contains(super::CLEAR_ALL),
            "image insert-before paths must force baseline fallback, got: {with_image_output:?}"
        );
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

        assert!(
            !render(&mut renderer).is_empty(),
            "expected first render output"
        );

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

    #[test]
    fn apply_out_of_band_move_by_clamps_to_viewport() {
        let mut renderer = DiffRenderer::new();

        let height = 3;
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        renderer.render(lines.clone().into(), 80, height, false, false);

        assert_eq!(renderer.previous_lines_len(), 10);
        assert_eq!(renderer.max_lines_rendered(), 10);

        let viewport_top = renderer.max_lines_rendered().saturating_sub(height);
        let viewport_bottom = viewport_top + height.saturating_sub(1);
        assert!(viewport_top > 0, "expected viewport_top > 0 for this test");

        renderer.apply_out_of_band_move_by(-1000, height);
        assert_eq!(renderer.hardware_cursor_row(), viewport_top);
        assert_eq!(
            renderer.previous_lines_len(),
            10,
            "cursor moves must not touch lines"
        );

        renderer.apply_out_of_band_move_by(1000, height);
        assert_eq!(renderer.hardware_cursor_row(), viewport_bottom);
        assert_eq!(
            renderer.previous_lines_len(),
            10,
            "cursor moves must not touch lines"
        );
    }

    #[test]
    fn prepend_growth_keeps_tail_viewport_cursor_clamp_deterministic() {
        let mut renderer = DiffRenderer::new();
        let height = 3;

        let base_lines: Vec<String> = (0..6).map(|i| format!("line-{i}")).collect();
        renderer.render(base_lines.clone().into(), 80, height, false, false);

        let mut grown_lines = vec!["history-a".to_string(), "history-b".to_string()];
        grown_lines.extend(base_lines);
        let output = cmds_to_bytes(renderer.render(grown_lines.into(), 80, height, false, false));

        assert!(
            output.contains("history-a") && output.contains("history-b"),
            "expected emitted bytes to include prepended history lines, got: {output:?}"
        );
        assert_eq!(renderer.previous_lines_len(), 8);
        assert_eq!(renderer.max_lines_rendered(), 8);

        let viewport_top = renderer.max_lines_rendered().saturating_sub(height);
        renderer.apply_out_of_band_move_by(-1000, height);
        assert_eq!(renderer.hardware_cursor_row(), viewport_top);

        renderer.apply_out_of_band_move_by(1000, height);
        assert_eq!(
            renderer.hardware_cursor_row(),
            viewport_top + height.saturating_sub(1)
        );
    }

    #[test]
    fn reset_for_external_clear_screen_causes_next_render_to_behave_like_first_render() {
        let mut renderer = DiffRenderer::new();

        let frame: Frame = vec!["one".to_string(), "two".to_string()].into();
        let out1 = cmds_to_bytes(renderer.render(frame.clone(), 20, 5, false, false));
        assert!(!out1.is_empty());
        assert_eq!(renderer.previous_lines_len(), 2);

        renderer.reset_for_external_clear_screen();

        let out2 = cmds_to_bytes(renderer.render(frame, 20, 5, false, false));
        assert!(
            !out2.is_empty(),
            "expected full output after external reset"
        );
        assert!(!out2.contains(super::CLEAR_ALL));
        assert!(!out2.contains("\x1b[3J"));
    }
}
