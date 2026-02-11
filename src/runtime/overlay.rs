//! Runtime-owned overlay identifiers and options.

use crate::render::overlay as render_overlay;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OverlayId(u64);

impl OverlayId {
    pub fn raw(self) -> u64 {
        self.0
    }

    pub(crate) fn from_raw(raw: u64) -> Self {
        Self(raw)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayVisibility {
    Always,
    MinCols(usize),
    MinSize { cols: usize, rows: usize },
}

impl Default for OverlayVisibility {
    fn default() -> Self {
        Self::Always
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
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
