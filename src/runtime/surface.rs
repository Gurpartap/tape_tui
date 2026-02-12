//! Runtime-owned surface primitives.
//!
//! Surfaces are transient UI layers composed above the scrolling root transcript.
//! They provide deterministic lifecycle, visibility, input policy, and lane semantics
//! while preserving inline-first rendering behavior.

use crate::render::overlay as render_overlay;
use crate::runtime::component_registry::ComponentId;

/// Stable identifier for a surface owned by a single runtime instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SurfaceId(u64);

impl SurfaceId {
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
pub enum SurfaceSizeValue {
    /// Absolute size in terminal cells.
    Absolute(usize),
    /// Relative size in percent (`0.0..=100.0` is typical).
    Percent(f32),
}

impl SurfaceSizeValue {
    /// Creates an absolute size.
    pub fn absolute(value: usize) -> Self {
        Self::Absolute(value)
    }

    /// Creates a percentage-based size.
    pub fn percent(value: f32) -> Self {
        Self::Percent(value)
    }
}

/// Surface anchoring positions inside the available terminal area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAnchor {
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

/// Optional non-negative margins around surface layout bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceMargin {
    /// Top margin in cells.
    pub top: Option<usize>,
    /// Right margin in cells.
    pub right: Option<usize>,
    /// Bottom margin in cells.
    pub bottom: Option<usize>,
    /// Left margin in cells.
    pub left: Option<usize>,
}

impl SurfaceMargin {
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

/// Visibility policy for showing surfaces at a given terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceVisibility {
    /// Always visible.
    Always,
    /// Visible only when terminal columns are at least this value.
    MinCols(usize),
    /// Visible only when both dimensions satisfy minimum requirements.
    MinSize { cols: usize, rows: usize },
}

impl Default for SurfaceVisibility {
    fn default() -> Self {
        Self::Always
    }
}

/// Runtime-level surface layout and visibility options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceLayoutOptions {
    /// Preferred width.
    pub width: Option<SurfaceSizeValue>,
    /// Lower bound for width after resolving `width`.
    pub min_width: Option<usize>,
    /// Maximum rendered height.
    pub max_height: Option<SurfaceSizeValue>,
    /// Anchor used when `row`/`col` are not explicitly set.
    pub anchor: Option<SurfaceAnchor>,
    /// Horizontal offset applied after resolving anchor/position.
    pub offset_x: Option<i32>,
    /// Vertical offset applied after resolving anchor/position.
    pub offset_y: Option<i32>,
    /// Explicit row position.
    pub row: Option<SurfaceSizeValue>,
    /// Explicit column position.
    pub col: Option<SurfaceSizeValue>,
    /// Optional margins inside the terminal bounds.
    pub margin: Option<SurfaceMargin>,
    /// Visibility policy evaluated per render.
    pub visibility: SurfaceVisibility,
}

impl Default for SurfaceLayoutOptions {
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
            visibility: SurfaceVisibility::Always,
        }
    }
}

impl SurfaceLayoutOptions {
    /// Returns whether this layout should be visible at the given terminal size.
    pub fn is_visible(&self, columns: usize, rows: usize) -> bool {
        match self.visibility {
            SurfaceVisibility::Always => true,
            SurfaceVisibility::MinCols(min_cols) => columns >= min_cols,
            SurfaceVisibility::MinSize {
                cols,
                rows: min_rows,
            } => columns >= cols && rows >= min_rows,
        }
    }
}

impl From<SurfaceSizeValue> for render_overlay::SizeValue {
    fn from(value: SurfaceSizeValue) -> Self {
        match value {
            SurfaceSizeValue::Absolute(value) => Self::Absolute(value),
            SurfaceSizeValue::Percent(value) => Self::Percent(value),
        }
    }
}

impl From<SurfaceAnchor> for render_overlay::OverlayAnchor {
    fn from(anchor: SurfaceAnchor) -> Self {
        match anchor {
            SurfaceAnchor::Center => Self::Center,
            SurfaceAnchor::TopLeft => Self::TopLeft,
            SurfaceAnchor::TopRight => Self::TopRight,
            SurfaceAnchor::BottomLeft => Self::BottomLeft,
            SurfaceAnchor::BottomRight => Self::BottomRight,
            SurfaceAnchor::TopCenter => Self::TopCenter,
            SurfaceAnchor::BottomCenter => Self::BottomCenter,
            SurfaceAnchor::LeftCenter => Self::LeftCenter,
            SurfaceAnchor::RightCenter => Self::RightCenter,
        }
    }
}

impl From<SurfaceMargin> for render_overlay::OverlayMargin {
    fn from(margin: SurfaceMargin) -> Self {
        Self {
            top: margin.top,
            right: margin.right,
            bottom: margin.bottom,
            left: margin.left,
        }
    }
}

impl From<&SurfaceLayoutOptions> for render_overlay::OverlayOptions {
    fn from(options: &SurfaceLayoutOptions) -> Self {
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

/// Surface class used to determine compositing lane defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// Centered or explicitly positioned modal-style layer.
    Modal,
    /// Bottom docked panel that typically captures input.
    Drawer,
    /// Small anchored info/status panel.
    Corner,
    /// Ephemeral top lane message.
    Toast,
    /// Bottom lane for attachments / chips / status rows.
    AttachmentRow,
}

impl Default for SurfaceKind {
    fn default() -> Self {
        Self::Modal
    }
}

/// Input routing policy for visible surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceInputPolicy {
    /// Surface receives input before root content.
    Capture,
    /// Surface is visual-only; input falls through to root/focused component.
    Passthrough,
}

impl Default for SurfaceInputPolicy {
    fn default() -> Self {
        Self::Capture
    }
}

/// Surface options composed from surface-native layout primitives plus surface semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceOptions {
    /// Layout and visibility controls for surface placement.
    pub overlay: SurfaceLayoutOptions,
    /// Surface class used for lane defaults and reservation behavior.
    pub kind: SurfaceKind,
    /// Input routing behavior.
    pub input_policy: SurfaceInputPolicy,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            overlay: SurfaceLayoutOptions::default(),
            kind: SurfaceKind::default(),
            input_policy: SurfaceInputPolicy::default(),
        }
    }
}

impl SurfaceOptions {
    /// Whether this surface should be visible at the provided terminal size.
    pub fn is_visible(&self, columns: usize, rows: usize) -> bool {
        self.overlay.is_visible(columns, rows)
    }

    /// Return layout options adjusted for lane reservations and surface defaults.
    ///
    /// Reasoning:
    /// - We keep geometry resolution in one place so compositing and input hit-targeting
    ///   are derived from the same options.
    /// - Lane reservations are additive margins; they do not mutate transcript ordering.
    pub fn with_lane_reservations(
        &self,
        reserved_top: usize,
        reserved_bottom: usize,
    ) -> SurfaceLayoutOptions {
        let mut layout = self.overlay;

        if reserved_top > 0 || reserved_bottom > 0 {
            let mut margin = layout.margin.unwrap_or(SurfaceMargin {
                top: None,
                right: None,
                bottom: None,
                left: None,
            });
            if reserved_top > 0 {
                margin.top = Some(margin.top.unwrap_or(0).saturating_add(reserved_top));
            }
            if reserved_bottom > 0 {
                margin.bottom = Some(margin.bottom.unwrap_or(0).saturating_add(reserved_bottom));
            }
            layout.margin = Some(margin);
        }

        match self.kind {
            SurfaceKind::Modal => {}
            SurfaceKind::Drawer => {
                if layout.anchor.is_none() && layout.row.is_none() {
                    layout.anchor = Some(SurfaceAnchor::BottomCenter);
                }
            }
            SurfaceKind::Corner => {
                if layout.anchor.is_none() && layout.row.is_none() && layout.col.is_none() {
                    layout.anchor = Some(SurfaceAnchor::BottomRight);
                }
            }
            SurfaceKind::Toast => {
                if layout.row.is_none() {
                    layout.row = Some(SurfaceSizeValue::absolute(0));
                }
                if layout.anchor.is_none() && layout.col.is_none() {
                    layout.anchor = Some(SurfaceAnchor::TopRight);
                }
            }
            SurfaceKind::AttachmentRow => {
                if layout.anchor.is_none() && layout.row.is_none() {
                    layout.anchor = Some(SurfaceAnchor::BottomLeft);
                }
            }
        }

        layout
    }
}

impl From<SurfaceLayoutOptions> for SurfaceOptions {
    fn from(overlay: SurfaceLayoutOptions) -> Self {
        Self {
            overlay,
            kind: SurfaceKind::Modal,
            input_policy: SurfaceInputPolicy::Capture,
        }
    }
}

impl From<&SurfaceLayoutOptions> for SurfaceOptions {
    fn from(overlay: &SurfaceLayoutOptions) -> Self {
        Self::from(*overlay)
    }
}

impl From<SurfaceOptions> for SurfaceLayoutOptions {
    fn from(options: SurfaceOptions) -> Self {
        options.overlay
    }
}

impl From<&SurfaceOptions> for SurfaceLayoutOptions {
    fn from(options: &SurfaceOptions) -> Self {
        options.overlay
    }
}

/// Runtime-owned surface entry.
#[derive(Clone, Copy)]
pub(crate) struct SurfaceEntry {
    pub(crate) id: SurfaceId,
    pub(crate) component_id: ComponentId,
    pub(crate) options: Option<SurfaceOptions>,
    pub(crate) pre_focus: Option<ComponentId>,
    pub(crate) hidden: bool,
}

impl SurfaceEntry {
    pub(crate) fn input_policy(&self) -> SurfaceInputPolicy {
        self.options
            .map_or(SurfaceInputPolicy::Capture, |options| options.input_policy)
    }

    pub(crate) fn is_visible(&self, columns: usize, rows: usize) -> bool {
        if self.hidden {
            return false;
        }
        self.options
            .map_or(true, |options| options.is_visible(columns, rows))
    }
}

/// Render-time snapshot entry.
#[derive(Clone, Copy)]
pub(crate) struct SurfaceRenderEntry {
    pub(crate) component_id: ComponentId,
    pub(crate) options: Option<SurfaceOptions>,
}

/// Ordered runtime surface stack.
#[derive(Default)]
pub(crate) struct SurfaceState {
    pub(crate) entries: Vec<SurfaceEntry>,
}

impl SurfaceState {
    pub(crate) fn index_of(&self, surface_id: SurfaceId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == surface_id)
    }

    pub(crate) fn contains(&self, surface_id: SurfaceId) -> bool {
        self.index_of(surface_id).is_some()
    }

    pub(crate) fn has_visible(&self, columns: usize, rows: usize) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.is_visible(columns, rows))
    }

    pub(crate) fn topmost_visible_component(
        &self,
        columns: usize,
        rows: usize,
        capture_only: bool,
    ) -> Option<ComponentId> {
        self.entries.iter().rev().find_map(|entry| {
            if !entry.is_visible(columns, rows) {
                return None;
            }
            if capture_only && entry.input_policy() != SurfaceInputPolicy::Capture {
                return None;
            }
            Some(entry.component_id)
        })
    }

    pub(crate) fn visible_snapshot(&self, columns: usize, rows: usize) -> Vec<SurfaceRenderEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.is_visible(columns, rows))
            .map(|entry| SurfaceRenderEntry {
                component_id: entry.component_id,
                options: entry.options,
            })
            .collect()
    }
}

/// Convenience helper to build surface options from a layout configuration.
pub fn surface_options_from_layout(layout: SurfaceLayoutOptions) -> SurfaceOptions {
    SurfaceOptions::from(layout)
}

/// Convenience helper to create surface options from visibility-only defaults.
pub fn visibility_only_surface_options(visibility: SurfaceVisibility) -> SurfaceOptions {
    let mut options = SurfaceOptions::default();
    options.overlay.visibility = visibility;
    options
}

#[cfg(test)]
mod tests {
    use super::{
        surface_options_from_layout, SurfaceAnchor, SurfaceInputPolicy, SurfaceKind,
        SurfaceLayoutOptions, SurfaceMargin, SurfaceOptions, SurfaceSizeValue, SurfaceVisibility,
    };

    #[test]
    fn surface_layout_visibility_matrix_is_deterministic() {
        let layout = SurfaceLayoutOptions {
            visibility: SurfaceVisibility::MinSize { cols: 80, rows: 24 },
            ..Default::default()
        };

        assert!(layout.is_visible(80, 24));
        assert!(layout.is_visible(120, 40));
        assert!(!layout.is_visible(79, 24));
        assert!(!layout.is_visible(80, 23));
    }

    #[test]
    fn lane_reservations_adjust_margins_without_mutating_original_layout() {
        let options = SurfaceOptions {
            overlay: SurfaceLayoutOptions {
                margin: Some(SurfaceMargin::uniform(1)),
                ..Default::default()
            },
            kind: SurfaceKind::Modal,
            input_policy: SurfaceInputPolicy::Capture,
        };

        let adjusted = options.with_lane_reservations(2, 3);
        assert_eq!(
            adjusted.margin,
            Some(SurfaceMargin {
                top: Some(3),
                right: Some(1),
                bottom: Some(4),
                left: Some(1),
            })
        );

        assert_eq!(options.overlay.margin, Some(SurfaceMargin::uniform(1)));
    }

    #[test]
    fn kind_defaults_apply_expected_anchor_and_row_defaults() {
        let drawer = SurfaceOptions {
            kind: SurfaceKind::Drawer,
            ..Default::default()
        }
        .with_lane_reservations(0, 0);
        assert_eq!(drawer.anchor, Some(SurfaceAnchor::BottomCenter));

        let corner = SurfaceOptions {
            kind: SurfaceKind::Corner,
            ..Default::default()
        }
        .with_lane_reservations(0, 0);
        assert_eq!(corner.anchor, Some(SurfaceAnchor::BottomRight));

        let toast = SurfaceOptions {
            kind: SurfaceKind::Toast,
            ..Default::default()
        }
        .with_lane_reservations(0, 0);
        assert_eq!(toast.row, Some(SurfaceSizeValue::Absolute(0)));
        assert_eq!(toast.anchor, Some(SurfaceAnchor::TopRight));

        let attachment = SurfaceOptions {
            kind: SurfaceKind::AttachmentRow,
            ..Default::default()
        }
        .with_lane_reservations(0, 0);
        assert_eq!(attachment.anchor, Some(SurfaceAnchor::BottomLeft));
    }

    #[test]
    fn layout_round_trip_preserves_resolution() {
        let layout = SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::percent(55.0)),
            min_width: Some(24),
            max_height: Some(SurfaceSizeValue::percent(60.0)),
            anchor: Some(SurfaceAnchor::BottomRight),
            offset_x: Some(-2),
            offset_y: Some(1),
            margin: Some(SurfaceMargin {
                top: Some(2),
                right: Some(3),
                bottom: Some(1),
                left: Some(4),
            }),
            ..Default::default()
        };

        let layout_render = crate::render::overlay::OverlayOptions::from(&layout);
        let expected =
            crate::render::overlay::resolve_overlay_layout(Some(&layout_render), 9, 120, 40);

        let surface_options = surface_options_from_layout(layout);
        let layout_options = surface_options.with_lane_reservations(0, 0);
        let surface_render = crate::render::overlay::OverlayOptions::from(&layout_options);
        let actual =
            crate::render::overlay::resolve_overlay_layout(Some(&surface_render), 9, 120, 40);

        assert_eq!(actual, expected);
        let round_trip_layout = SurfaceLayoutOptions::from(surface_options);
        assert_eq!(round_trip_layout, layout);
    }
}
