//! Runtime-owned overlay identifiers and options.
//!
//! Overlays are legacy-compatible wrappers over runtime surfaces.
//! New host integrations should prefer `runtime::surface` APIs.

use crate::render::overlay as render_overlay;

/// Stable identifier for an overlay owned by a single runtime instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OverlayId(u64);

impl OverlayId {
    /// Returns the raw numeric identifier.
    pub fn raw(self) -> u64 {
        self.0
    }

    pub(crate) fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Dimension value represented as absolute cells or percent of terminal size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeValue {
    /// Absolute size in terminal cells.
    Absolute(usize),
    /// Relative size in percent (`0.0..=100.0` is typical).
    Percent(f32),
}

impl SizeValue {
    /// Creates an absolute size.
    pub fn absolute(value: usize) -> Self {
        Self::Absolute(value)
    }

    /// Creates a percentage-based size.
    pub fn percent(value: f32) -> Self {
        Self::Percent(value)
    }
}

/// Overlay anchoring positions inside the available terminal area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    /// Centered horizontally and vertically.
    Center,
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Top edge, horizontally centered.
    TopCenter,
    /// Bottom edge, horizontally centered.
    BottomCenter,
    /// Left edge, vertically centered.
    LeftCenter,
    /// Right edge, vertically centered.
    RightCenter,
}

/// Optional non-negative margins around overlay layout bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayMargin {
    /// Top margin in cells.
    pub top: Option<usize>,
    /// Right margin in cells.
    pub right: Option<usize>,
    /// Bottom margin in cells.
    pub bottom: Option<usize>,
    /// Left margin in cells.
    pub left: Option<usize>,
}

impl OverlayMargin {
    /// Creates a uniform margin on all sides.
    pub fn uniform(value: usize) -> Self {
        Self {
            top: Some(value),
            right: Some(value),
            bottom: Some(value),
            left: Some(value),
        }
    }
}

/// Visibility policy for showing overlays at a given terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayVisibility {
    /// Always visible.
    Always,
    /// Visible only when terminal columns are at least this value.
    MinCols(usize),
    /// Visible only when both dimensions satisfy minimum requirements.
    MinSize { cols: usize, rows: usize },
}

impl Default for OverlayVisibility {
    fn default() -> Self {
        Self::Always
    }
}

/// Runtime-level overlay layout and visibility options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayOptions {
    /// Preferred width.
    pub width: Option<SizeValue>,
    /// Lower bound for width after resolving `width`.
    pub min_width: Option<usize>,
    /// Maximum rendered height.
    pub max_height: Option<SizeValue>,
    /// Anchor used when `row`/`col` are not explicitly set.
    pub anchor: Option<OverlayAnchor>,
    /// Horizontal offset applied after resolving anchor/position.
    pub offset_x: Option<i32>,
    /// Vertical offset applied after resolving anchor/position.
    pub offset_y: Option<i32>,
    /// Explicit row position.
    pub row: Option<SizeValue>,
    /// Explicit column position.
    pub col: Option<SizeValue>,
    /// Optional margins inside the terminal bounds.
    pub margin: Option<OverlayMargin>,
    /// Visibility policy evaluated per render.
    pub visibility: OverlayVisibility,
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
            visibility: OverlayVisibility::Always,
        }
    }
}

impl OverlayOptions {
    /// Returns whether this overlay should be visible at the given terminal size.
    pub fn is_visible(&self, columns: usize, rows: usize) -> bool {
        match self.visibility {
            OverlayVisibility::Always => true,
            OverlayVisibility::MinCols(min_cols) => columns >= min_cols,
            OverlayVisibility::MinSize {
                cols,
                rows: min_rows,
            } => columns >= cols && rows >= min_rows,
        }
    }
}

/// Returns visibility for optional overlay options (`true` when `None`).
pub fn is_overlay_visible(options: Option<&OverlayOptions>, columns: usize, rows: usize) -> bool {
    options.map_or(true, |options| options.is_visible(columns, rows))
}

impl From<SizeValue> for render_overlay::SizeValue {
    fn from(value: SizeValue) -> Self {
        match value {
            SizeValue::Absolute(value) => Self::Absolute(value),
            SizeValue::Percent(value) => Self::Percent(value),
        }
    }
}

impl From<OverlayAnchor> for render_overlay::OverlayAnchor {
    fn from(anchor: OverlayAnchor) -> Self {
        match anchor {
            OverlayAnchor::Center => Self::Center,
            OverlayAnchor::TopLeft => Self::TopLeft,
            OverlayAnchor::TopRight => Self::TopRight,
            OverlayAnchor::BottomLeft => Self::BottomLeft,
            OverlayAnchor::BottomRight => Self::BottomRight,
            OverlayAnchor::TopCenter => Self::TopCenter,
            OverlayAnchor::BottomCenter => Self::BottomCenter,
            OverlayAnchor::LeftCenter => Self::LeftCenter,
            OverlayAnchor::RightCenter => Self::RightCenter,
        }
    }
}

impl From<OverlayMargin> for render_overlay::OverlayMargin {
    fn from(margin: OverlayMargin) -> Self {
        Self {
            top: margin.top,
            right: margin.right,
            bottom: margin.bottom,
            left: margin.left,
        }
    }
}

impl From<&OverlayOptions> for render_overlay::OverlayOptions {
    fn from(options: &OverlayOptions) -> Self {
        Self {
            width: options.width.map(Into::into),
            min_width: options.min_width,
            max_height: options.max_height.map(Into::into),
            anchor: options.anchor.map(Into::into),
            offset_x: options.offset_x,
            offset_y: options.offset_y,
            row: options.row.map(Into::into),
            col: options.col.map(Into::into),
            margin: options.margin.map(Into::into),
            visible: None,
        }
    }
}
