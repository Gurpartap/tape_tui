//! Overlay compositing (Phase 7).

use crate::render::slice::{extract_segments, slice_by_column, slice_with_width};
use crate::render::width::visible_width;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayMargin {
    pub top: Option<usize>,
    pub right: Option<usize>,
    pub bottom: Option<usize>,
    pub left: Option<usize>,
}

impl OverlayMargin {
    pub fn uniform(value: usize) -> Self {
        Self {
            top: Some(value),
            right: Some(value),
            bottom: Some(value),
            left: Some(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeValue {
    Absolute(usize),
    Percent(f32),
}

impl SizeValue {
    pub fn absolute(value: usize) -> Self {
        Self::Absolute(value)
    }

    pub fn percent(value: f32) -> Self {
        Self::Percent(value)
    }
}

pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<usize>,
    pub max_height: Option<SizeValue>,
    pub anchor: Option<OverlayAnchor>,
    pub offset_x: Option<i32>,
    pub offset_y: Option<i32>,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: Option<OverlayMargin>,
    pub visible: Option<Box<dyn Fn(usize, usize) -> bool>>,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            min_width: None,
            max_height: None,
            anchor: None,
            offset_x: None,
            offset_y: None,
            row: None,
            col: None,
            margin: None,
            visible: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayLayout {
    pub width: usize,
    pub row: usize,
    pub col: usize,
    pub max_height: Option<usize>,
}

#[derive(Debug)]
pub struct RenderedOverlay {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    pub width: usize,
}

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

fn parse_size_value(value: Option<SizeValue>, reference: usize) -> Option<usize> {
    match value {
        None => None,
        Some(SizeValue::Absolute(v)) => Some(v),
        Some(SizeValue::Percent(percent)) => {
            let percent = percent.max(0.0);
            Some(((reference as f32) * (percent / 100.0)).floor() as usize)
        }
    }
}

pub fn resolve_overlay_layout(
    options: Option<&OverlayOptions>,
    overlay_height: usize,
    term_width: usize,
    term_height: usize,
) -> OverlayLayout {
    let default_options = OverlayOptions::default();
    let opt = options.unwrap_or(&default_options);

    let margin = opt.margin.unwrap_or(OverlayMargin {
        top: None,
        right: None,
        bottom: None,
        left: None,
    });
    let margin_top = margin.top.unwrap_or(0);
    let margin_right = margin.right.unwrap_or(0);
    let margin_bottom = margin.bottom.unwrap_or(0);
    let margin_left = margin.left.unwrap_or(0);

    let avail_width = term_width.saturating_sub(margin_left + margin_right).max(1);
    let avail_height = term_height.saturating_sub(margin_top + margin_bottom).max(1);

    let mut width = parse_size_value(opt.width, term_width).unwrap_or_else(|| 80.min(avail_width));
    if let Some(min_width) = opt.min_width {
        width = width.max(min_width);
    }
    width = width.clamp(1, avail_width);

    let mut max_height = parse_size_value(opt.max_height, term_height);
    if let Some(height) = max_height.as_mut() {
        *height = (*height).clamp(1, avail_height);
    }

    let effective_height = max_height.map_or(overlay_height, |height| overlay_height.min(height));

    let mut row = if let Some(value) = opt.row {
        match value {
            SizeValue::Absolute(v) => v,
            SizeValue::Percent(percent) => {
                let max_row = avail_height.saturating_sub(effective_height);
                let percent = percent.max(0.0);
                margin_top + ((max_row as f32) * (percent / 100.0)).floor() as usize
            }
        }
    } else {
        let anchor = opt.anchor.unwrap_or(OverlayAnchor::Center);
        resolve_anchor_row(anchor, effective_height, avail_height, margin_top)
    };

    let mut col = if let Some(value) = opt.col {
        match value {
            SizeValue::Absolute(v) => v,
            SizeValue::Percent(percent) => {
                let max_col = avail_width.saturating_sub(width);
                let percent = percent.max(0.0);
                margin_left + ((max_col as f32) * (percent / 100.0)).floor() as usize
            }
        }
    } else {
        let anchor = opt.anchor.unwrap_or(OverlayAnchor::Center);
        resolve_anchor_col(anchor, width, avail_width, margin_left)
    };

    if let Some(offset) = opt.offset_y {
        row = apply_offset(row, offset);
    }
    if let Some(offset) = opt.offset_x {
        col = apply_offset(col, offset);
    }

    let max_row = term_height.saturating_sub(margin_bottom + effective_height);
    row = row.clamp(margin_top, max_row);
    let max_col = term_width.saturating_sub(margin_right + width);
    col = col.clamp(margin_left, max_col);

    OverlayLayout {
        width,
        row,
        col,
        max_height,
    }
}

pub fn composite_overlays(
    lines: Vec<String>,
    overlays: &[RenderedOverlay],
    term_width: usize,
    term_height: usize,
    max_lines_rendered: usize,
    is_image_line: fn(&str) -> bool,
) -> Vec<String> {
    if overlays.is_empty() {
        return lines;
    }

    let mut result = lines;
    let mut min_lines_needed = result.len();
    for overlay in overlays {
        min_lines_needed = min_lines_needed.max(overlay.row + overlay.lines.len());
    }

    let working_height = max_lines_rendered.max(min_lines_needed);
    while result.len() < working_height {
        result.push(String::new());
    }

    let viewport_start = working_height.saturating_sub(term_height);
    let mut modified_lines = Vec::new();

    for overlay in overlays {
        for (i, line) in overlay.lines.iter().enumerate() {
            let idx = viewport_start + overlay.row + i;
            if idx < result.len() {
                let truncated = if visible_width(line) > overlay.width {
                    slice_by_column(line, 0, overlay.width, true)
                } else {
                    line.clone()
                };
                let composed = composite_line_at(
                    &result[idx],
                    &truncated,
                    overlay.col,
                    overlay.width,
                    term_width,
                    is_image_line,
                );
                result[idx] = composed;
                modified_lines.push(idx);
            }
        }
    }

    modified_lines.sort_unstable();
    modified_lines.dedup();
    for idx in modified_lines {
        if visible_width(&result[idx]) > term_width {
            result[idx] = slice_by_column(&result[idx], 0, term_width, true);
        }
    }

    result
}

pub fn composite_line_at(
    base_line: &str,
    overlay_line: &str,
    start_col: usize,
    overlay_width: usize,
    total_width: usize,
    is_image_line: fn(&str) -> bool,
) -> String {
    if is_image_line(base_line) {
        return base_line.to_string();
    }

    let after_start = start_col.saturating_add(overlay_width);
    let base = extract_segments(base_line, start_col, after_start, total_width.saturating_sub(after_start), true);
    let overlay = slice_with_width(overlay_line, 0, overlay_width, true);

    let before_pad = start_col.saturating_sub(base.before_width);
    let overlay_pad = overlay_width.saturating_sub(overlay.width);
    let actual_before_width = start_col.max(base.before_width);
    let actual_overlay_width = overlay_width.max(overlay.width);
    let after_target = total_width.saturating_sub(actual_before_width + actual_overlay_width);
    let after_pad = after_target.saturating_sub(base.after_width);

    let mut result = String::new();
    result.push_str(&base.before);
    result.push_str(&" ".repeat(before_pad));
    result.push_str(SEGMENT_RESET);
    result.push_str(&overlay.text);
    result.push_str(&" ".repeat(overlay_pad));
    result.push_str(SEGMENT_RESET);
    result.push_str(&base.after);
    result.push_str(&" ".repeat(after_pad));

    if visible_width(&result) <= total_width {
        return result;
    }

    slice_by_column(&result, 0, total_width, true)
}

fn resolve_anchor_row(anchor: OverlayAnchor, height: usize, avail_height: usize, margin_top: usize) -> usize {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::TopCenter | OverlayAnchor::TopRight => margin_top,
        OverlayAnchor::BottomLeft | OverlayAnchor::BottomCenter | OverlayAnchor::BottomRight => {
            margin_top + avail_height.saturating_sub(height)
        }
        OverlayAnchor::LeftCenter | OverlayAnchor::Center | OverlayAnchor::RightCenter => {
            margin_top + avail_height.saturating_sub(height) / 2
        }
    }
}

fn resolve_anchor_col(anchor: OverlayAnchor, width: usize, avail_width: usize, margin_left: usize) -> usize {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::LeftCenter | OverlayAnchor::BottomLeft => margin_left,
        OverlayAnchor::TopRight | OverlayAnchor::RightCenter | OverlayAnchor::BottomRight => {
            margin_left + avail_width.saturating_sub(width)
        }
        OverlayAnchor::TopCenter | OverlayAnchor::Center | OverlayAnchor::BottomCenter => {
            margin_left + avail_width.saturating_sub(width) / 2
        }
    }
}

fn apply_offset(value: usize, offset: i32) -> usize {
    if offset >= 0 {
        value.saturating_add(offset as usize)
    } else {
        value.saturating_sub(offset.unsigned_abs() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::width::visible_width;

    fn not_image(_: &str) -> bool {
        false
    }

    #[test]
    fn layout_percent_positioning() {
        let mut options = OverlayOptions::default();
        options.row = Some(SizeValue::Percent(50.0));
        options.col = Some(SizeValue::Percent(50.0));
        let layout = resolve_overlay_layout(Some(&options), 2, 20, 10);
        assert_eq!(layout.row, 4);
        assert_eq!(layout.col, 0);
    }

    #[test]
    fn composite_preserves_styles_and_width() {
        let base = "base";
        let overlay = "\x1b[31mred\x1b[0m";
        let composed = composite_line_at(base, overlay, 0, 3, 4, not_image);
        assert!(composed.contains("red"));
        assert!(visible_width(&composed) <= 4);
    }
}
