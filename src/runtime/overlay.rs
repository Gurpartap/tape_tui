//! Runtime-owned overlay identifiers and legacy layout aliases.
//!
//! Overlay geometry types are compatibility aliases to the surface-native layout model.
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

/// Compatibility alias for surface-native size values.
pub type SizeValue = crate::runtime::surface::SurfaceSizeValue;
/// Compatibility alias for surface-native anchor values.
pub type OverlayAnchor = crate::runtime::surface::SurfaceAnchor;
/// Compatibility alias for surface-native margins.
pub type OverlayMargin = crate::runtime::surface::SurfaceMargin;
/// Compatibility alias for surface-native visibility rules.
pub type OverlayVisibility = crate::runtime::surface::SurfaceVisibility;
/// Compatibility alias for surface-native layout options.
pub type OverlayOptions = crate::runtime::surface::SurfaceLayoutOptions;

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
