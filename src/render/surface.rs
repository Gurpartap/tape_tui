//! Surface compositing.

use crate::core::text::slice::{extract_segments, slice_by_column, slice_with_width};
use crate::core::text::width::visible_width;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAnchor {
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
pub struct SurfaceMargin {
    pub top: Option<usize>,
    pub right: Option<usize>,
    pub bottom: Option<usize>,
    pub left: Option<usize>,
}

impl SurfaceMargin {
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
pub enum SurfaceSizeValue {
    Absolute(usize),
    Percent(f32),
}

impl SurfaceSizeValue {
    pub fn absolute(value: usize) -> Self {
        Self::Absolute(value)
    }

    pub fn percent(value: f32) -> Self {
        Self::Percent(value)
    }
}

#[derive(Default)]
pub struct SurfaceOptions {
    pub width: Option<SurfaceSizeValue>,
    pub min_width: Option<usize>,
    pub max_height: Option<SurfaceSizeValue>,
    pub anchor: Option<SurfaceAnchor>,
    pub offset_x: Option<i32>,
    pub offset_y: Option<i32>,
    pub row: Option<SurfaceSizeValue>,
    pub col: Option<SurfaceSizeValue>,
    pub margin: Option<SurfaceMargin>,
    pub visible: Option<Box<dyn Fn(usize, usize) -> bool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceLayout {
    pub width: usize,
    pub row: usize,
    pub col: usize,
    pub max_height: Option<usize>,
}

#[derive(Debug)]
pub struct RenderedSurface {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    pub width: usize,
}

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

fn parse_size_value(value: Option<SurfaceSizeValue>, reference: usize) -> Option<usize> {
    match value {
        None => None,
        Some(SurfaceSizeValue::Absolute(v)) => Some(v),
        Some(SurfaceSizeValue::Percent(percent)) => {
            let percent = percent.max(0.0);
            Some(((reference as f32) * (percent / 100.0)).floor() as usize)
        }
    }
}

fn clamp_within(value: usize, min: usize, max: usize) -> usize {
    if min > max {
        max
    } else {
        value.clamp(min, max)
    }
}

pub fn resolve_surface_layout(
    options: Option<&SurfaceOptions>,
    surface_height: usize,
    term_width: usize,
    term_height: usize,
) -> SurfaceLayout {
    let default_options = SurfaceOptions::default();
    let opt = options.unwrap_or(&default_options);

    let margin = opt.margin.unwrap_or(SurfaceMargin {
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
    let avail_height = term_height
        .saturating_sub(margin_top + margin_bottom)
        .max(1);

    let mut width = parse_size_value(opt.width, term_width).unwrap_or_else(|| 80.min(avail_width));
    if let Some(min_width) = opt.min_width {
        width = width.max(min_width);
    }
    width = width.clamp(1, avail_width);

    let mut max_height = parse_size_value(opt.max_height, term_height);
    if let Some(height) = max_height.as_mut() {
        *height = (*height).clamp(1, avail_height);
    }

    let effective_height = max_height.map_or(surface_height, |height| surface_height.min(height));

    let mut row = if let Some(value) = opt.row {
        match value {
            SurfaceSizeValue::Absolute(v) => v,
            SurfaceSizeValue::Percent(percent) => {
                let max_row = avail_height.saturating_sub(effective_height);
                let percent = percent.max(0.0);
                margin_top + ((max_row as f32) * (percent / 100.0)).floor() as usize
            }
        }
    } else {
        let anchor = opt.anchor.unwrap_or(SurfaceAnchor::Center);
        resolve_anchor_row(anchor, effective_height, avail_height, margin_top)
    };

    let mut col = if let Some(value) = opt.col {
        match value {
            SurfaceSizeValue::Absolute(v) => v,
            SurfaceSizeValue::Percent(percent) => {
                let max_col = avail_width.saturating_sub(width);
                let percent = percent.max(0.0);
                margin_left + ((max_col as f32) * (percent / 100.0)).floor() as usize
            }
        }
    } else {
        let anchor = opt.anchor.unwrap_or(SurfaceAnchor::Center);
        resolve_anchor_col(anchor, width, avail_width, margin_left)
    };

    if let Some(offset) = opt.offset_y {
        row = apply_offset(row, offset);
    }
    if let Some(offset) = opt.offset_x {
        col = apply_offset(col, offset);
    }

    let max_row = term_height.saturating_sub(margin_bottom + effective_height);
    row = clamp_within(row, margin_top, max_row);
    let max_col = term_width.saturating_sub(margin_right + width);
    col = clamp_within(col, margin_left, max_col);

    SurfaceLayout {
        width,
        row,
        col,
        max_height,
    }
}

pub fn composite_surfaces(
    lines: Vec<String>,
    surfaces: &[RenderedSurface],
    term_width: usize,
    term_height: usize,
    max_lines_rendered: usize,
    is_image_line: fn(&str) -> bool,
) -> Vec<String> {
    if surfaces.is_empty() {
        return lines;
    }

    let mut result = lines;
    let mut min_lines_needed = result.len();
    for surface in surfaces {
        min_lines_needed = min_lines_needed.max(surface.row + surface.lines.len());
    }

    let working_height = max_lines_rendered.max(min_lines_needed);
    while result.len() < working_height {
        result.push(String::new());
    }

    let viewport_start = working_height.saturating_sub(term_height);
    let mut modified_lines = Vec::new();

    for surface in surfaces {
        for (i, line) in surface.lines.iter().enumerate() {
            let idx = viewport_start + surface.row + i;
            if idx < result.len() {
                let truncated = if visible_width(line) > surface.width {
                    slice_by_column(line, 0, surface.width, true)
                } else {
                    line.clone()
                };
                let composed = composite_line_at(
                    &result[idx],
                    &truncated,
                    surface.col,
                    surface.width,
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
    surface_line: &str,
    start_col: usize,
    surface_width: usize,
    total_width: usize,
    is_image_line: fn(&str) -> bool,
) -> String {
    if is_image_line(base_line) {
        return base_line.to_string();
    }

    let after_start = start_col.saturating_add(surface_width);
    let base = extract_segments(
        base_line,
        start_col,
        after_start,
        total_width.saturating_sub(after_start),
        true,
    );
    let surface = slice_with_width(surface_line, 0, surface_width, true);

    let before_pad = start_col.saturating_sub(base.before_width);
    let surface_pad = surface_width.saturating_sub(surface.width);
    let actual_before_width = start_col.max(base.before_width);
    let actual_surface_width = surface_width.max(surface.width);
    let after_target = total_width.saturating_sub(actual_before_width + actual_surface_width);
    let after_pad = after_target.saturating_sub(base.after_width);

    let mut result = String::new();
    result.push_str(&base.before);
    result.push_str(&" ".repeat(before_pad));
    result.push_str(SEGMENT_RESET);
    result.push_str(&surface.text);
    result.push_str(&" ".repeat(surface_pad));
    result.push_str(SEGMENT_RESET);
    result.push_str(&base.after);
    result.push_str(&" ".repeat(after_pad));

    if visible_width(&result) <= total_width {
        return result;
    }

    slice_by_column(&result, 0, total_width, true)
}

fn resolve_anchor_row(
    anchor: SurfaceAnchor,
    height: usize,
    avail_height: usize,
    margin_top: usize,
) -> usize {
    match anchor {
        SurfaceAnchor::TopLeft | SurfaceAnchor::TopCenter | SurfaceAnchor::TopRight => margin_top,
        SurfaceAnchor::BottomLeft | SurfaceAnchor::BottomCenter | SurfaceAnchor::BottomRight => {
            margin_top + avail_height.saturating_sub(height)
        }
        SurfaceAnchor::LeftCenter | SurfaceAnchor::Center | SurfaceAnchor::RightCenter => {
            margin_top + avail_height.saturating_sub(height) / 2
        }
    }
}

fn resolve_anchor_col(
    anchor: SurfaceAnchor,
    width: usize,
    avail_width: usize,
    margin_left: usize,
) -> usize {
    match anchor {
        SurfaceAnchor::TopLeft | SurfaceAnchor::LeftCenter | SurfaceAnchor::BottomLeft => {
            margin_left
        }
        SurfaceAnchor::TopRight | SurfaceAnchor::RightCenter | SurfaceAnchor::BottomRight => {
            margin_left + avail_width.saturating_sub(width)
        }
        SurfaceAnchor::TopCenter | SurfaceAnchor::Center | SurfaceAnchor::BottomCenter => {
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
    use crate::core::text::width::visible_width;

    fn not_image(_: &str) -> bool {
        false
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.match_indices(needle).count()
    }

    #[test]
    fn layout_anchor_matrix_all_variants() {
        let cases = [
            (SurfaceAnchor::TopLeft, 0, 0),
            (SurfaceAnchor::TopRight, 0, 14),
            (SurfaceAnchor::BottomLeft, 7, 0),
            (SurfaceAnchor::BottomRight, 7, 14),
            (SurfaceAnchor::TopCenter, 0, 7),
            (SurfaceAnchor::BottomCenter, 7, 7),
            (SurfaceAnchor::LeftCenter, 3, 0),
            (SurfaceAnchor::RightCenter, 3, 14),
            (SurfaceAnchor::Center, 3, 7),
        ];
        for (anchor, expected_row, expected_col) in cases {
            let options = SurfaceOptions {
                width: Some(SurfaceSizeValue::Absolute(6)),
                anchor: Some(anchor),
                ..Default::default()
            };
            let layout = resolve_surface_layout(Some(&options), 3, 20, 10);
            assert_eq!(layout.row, expected_row, "anchor {anchor:?} row mismatch");
            assert_eq!(layout.col, expected_col, "anchor {anchor:?} col mismatch");
        }
    }

    #[test]
    fn layout_percent_boundaries_and_clamping() {
        let cases = [
            (0.0, 0.0, 0, 0),
            (50.0, 50.0, 4, 6),
            (100.0, 100.0, 8, 12),
            (175.0, 250.0, 8, 12),
            (-25.0, -10.0, 0, 0),
        ];
        for (row_percent, col_percent, expected_row, expected_col) in cases {
            let options = SurfaceOptions {
                width: Some(SurfaceSizeValue::Absolute(8)),
                row: Some(SurfaceSizeValue::Percent(row_percent)),
                col: Some(SurfaceSizeValue::Percent(col_percent)),
                ..Default::default()
            };
            let layout = resolve_surface_layout(Some(&options), 2, 20, 10);
            assert_eq!(
                layout.row, expected_row,
                "row percent {row_percent} should resolve predictably"
            );
            assert_eq!(
                layout.col, expected_col,
                "col percent {col_percent} should resolve predictably"
            );
        }
    }

    #[test]
    fn layout_margin_and_size_constraints_interact_correctly() {
        let options = SurfaceOptions {
            width: Some(SurfaceSizeValue::Absolute(30)),
            min_width: Some(20),
            max_height: Some(SurfaceSizeValue::Percent(90.0)),
            anchor: Some(SurfaceAnchor::BottomRight),
            margin: Some(SurfaceMargin {
                top: Some(1),
                right: Some(3),
                bottom: Some(4),
                left: Some(2),
            }),
            ..Default::default()
        };
        let layout = resolve_surface_layout(Some(&options), 6, 20, 10);
        assert_eq!(layout.width, 15);
        assert_eq!(layout.max_height, Some(5));
        assert_eq!(layout.row, 1);
        assert_eq!(layout.col, 2);
    }

    #[test]
    fn layout_absolute_position_overrides_anchor_then_offsets_and_clamps() {
        let options = SurfaceOptions {
            width: Some(SurfaceSizeValue::Absolute(5)),
            anchor: Some(SurfaceAnchor::BottomRight),
            row: Some(SurfaceSizeValue::Absolute(2)),
            col: Some(SurfaceSizeValue::Absolute(1)),
            offset_y: Some(-10),
            offset_x: Some(50),
            margin: Some(SurfaceMargin::uniform(1)),
            ..Default::default()
        };
        let layout = resolve_surface_layout(Some(&options), 2, 20, 10);
        assert_eq!(layout.row, 1);
        assert_eq!(layout.col, 14);
    }

    #[test]
    fn composite_line_truncates_mixed_ansi_osc_surface_and_closes_segments() {
        let base = "0123456789";
        let surface = "\x1b[31mAB\x1b]8;;https://x\x07CDEFGH\x1b]8;;\x07\x1b[0m";
        let composed = composite_line_at(base, surface, 2, 6, 10, not_image);
        let expected =
            "01\x1b[0m\x1b]8;;\x07\x1b[31mAB\x1b]8;;https://x\x07CDEF\x1b[0m\x1b]8;;\x0789";
        assert_eq!(composed, expected);
        assert_eq!(visible_width(&composed), 10);
        assert_eq!(count_occurrences(&composed, SEGMENT_RESET), 2);
    }

    #[test]
    fn composite_line_pads_short_mixed_ansi_osc_surface() {
        let base = "abcdef";
        let surface = "\x1b]8;;https://x\x07Z\x1b]8;;\x07";
        let composed = composite_line_at(base, surface, 0, 4, 6, not_image);
        let expected =
            "\x1b[0m\x1b]8;;\x07\x1b]8;;https://x\x07Z\x1b]8;;\x07   \x1b[0m\x1b]8;;\x07ef";
        assert_eq!(composed, expected);
        assert_eq!(visible_width(&composed), 6);
    }

    #[test]
    fn composite_surfaces_inserts_reset_guards_for_style_safety() {
        let base = vec!["\x1b[3mXXXXXXXXXX\x1b[23m".to_string(), "INPUT".to_string()];
        let surfaces = vec![RenderedSurface {
            lines: vec!["OVR".to_string()],
            row: 0,
            col: 5,
            width: 3,
        }];
        let composed = composite_surfaces(base, &surfaces, 10, 2, 2, not_image);
        assert_eq!(composed.len(), 2);
        assert_eq!(visible_width(&composed[0]), 10);
        assert_eq!(count_occurrences(&composed[0], SEGMENT_RESET), 2);
        assert_eq!(composed[1], "INPUT");
    }
}
