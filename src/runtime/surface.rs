//! Runtime-owned surface primitives.
//!
//! Surfaces are transient UI layers composed above the scrolling root transcript.
//! They provide deterministic lifecycle, visibility, input policy, and lane semantics
//! while preserving inline-first rendering behavior.

use crate::render::surface as render_surface;
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

impl From<SurfaceSizeValue> for render_surface::SurfaceSizeValue {
    fn from(value: SurfaceSizeValue) -> Self {
        match value {
            SurfaceSizeValue::Absolute(value) => Self::Absolute(value),
            SurfaceSizeValue::Percent(value) => Self::Percent(value),
        }
    }
}

impl From<SurfaceAnchor> for render_surface::SurfaceAnchor {
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

impl From<SurfaceMargin> for render_surface::SurfaceMargin {
    fn from(margin: SurfaceMargin) -> Self {
        Self {
            top: margin.top,
            right: margin.right,
            bottom: margin.bottom,
            left: margin.left,
        }
    }
}

impl From<&SurfaceLayoutOptions> for render_surface::SurfaceOptions {
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

/// Deterministic compositing lane used for two-pass sizing negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceLane {
    Top,
    Bottom,
    Floating,
}

impl SurfaceLane {
    pub(crate) fn from_kind(kind: SurfaceKind) -> Self {
        match kind {
            SurfaceKind::Toast => Self::Top,
            SurfaceKind::AttachmentRow | SurfaceKind::Drawer => Self::Bottom,
            SurfaceKind::Modal | SurfaceKind::Corner => Self::Floating,
        }
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
    pub layout: SurfaceLayoutOptions,
    /// Surface class used for lane defaults and reservation behavior.
    pub kind: SurfaceKind,
    /// Input routing behavior.
    pub input_policy: SurfaceInputPolicy,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            layout: SurfaceLayoutOptions::default(),
            kind: SurfaceKind::default(),
            input_policy: SurfaceInputPolicy::default(),
        }
    }
}

impl SurfaceOptions {
    /// Whether this surface should be visible at the provided terminal size.
    pub fn is_visible(&self, columns: usize, rows: usize) -> bool {
        self.layout.is_visible(columns, rows)
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
        let mut layout = self.layout;

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
    fn from(layout: SurfaceLayoutOptions) -> Self {
        Self {
            layout,
            kind: SurfaceKind::Modal,
            input_policy: SurfaceInputPolicy::Capture,
        }
    }
}

impl From<&SurfaceLayoutOptions> for SurfaceOptions {
    fn from(layout: &SurfaceLayoutOptions) -> Self {
        Self::from(*layout)
    }
}

impl From<SurfaceOptions> for SurfaceLayoutOptions {
    fn from(options: SurfaceOptions) -> Self {
        options.layout
    }
}

impl From<&SurfaceOptions> for SurfaceLayoutOptions {
    fn from(options: &SurfaceOptions) -> Self {
        options.layout
    }
}

/// Internal ordered lifecycle mutation applied to runtime-managed surfaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SurfaceMutation {
    Show {
        surface_id: SurfaceId,
        component_id: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    },
    Hide {
        surface_id: SurfaceId,
    },
    SetHidden {
        surface_id: SurfaceId,
        hidden: bool,
    },
    UpdateOptions {
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    },
    BringToFront {
        surface_id: SurfaceId,
    },
    SendToBack {
        surface_id: SurfaceId,
    },
    Raise {
        surface_id: SurfaceId,
    },
    Lower {
        surface_id: SurfaceId,
    },
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

/// Deterministic first-pass measurement output for a visible surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceMeasurement {
    pub(crate) component_id: ComponentId,
    pub(crate) stack_index: usize,
    pub(crate) kind: SurfaceKind,
    pub(crate) lane: SurfaceLane,
    pub(crate) preferred_width: usize,
    pub(crate) preferred_max_height: Option<usize>,
}

/// Resolve deterministic first-pass measurement inputs for visible surfaces.
pub(crate) fn measure_visible_surfaces(
    entries: &[SurfaceRenderEntry],
    terminal_cols: usize,
    terminal_rows: usize,
) -> Vec<SurfaceMeasurement> {
    let terminal_cols = terminal_cols.max(1);
    let terminal_rows = terminal_rows.max(1);

    entries
        .iter()
        .enumerate()
        .map(|(stack_index, entry)| {
            let options = entry.options.unwrap_or_default();
            let layout = options.layout;

            let mut preferred_width = resolve_measurement_size(layout.width, terminal_cols)
                .unwrap_or_else(|| 80.min(terminal_cols));
            if let Some(min_width) = layout.min_width {
                preferred_width = preferred_width.max(min_width);
            }
            preferred_width = preferred_width.clamp(1, terminal_cols);

            let preferred_max_height = resolve_measurement_size(layout.max_height, terminal_rows)
                .map(|height| height.clamp(1, terminal_rows));

            SurfaceMeasurement {
                component_id: entry.component_id,
                stack_index,
                kind: options.kind,
                lane: SurfaceLane::from_kind(options.kind),
                preferred_width,
                preferred_max_height,
            }
        })
        .collect()
}

/// Deterministic second-pass allocation output for a measured surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceAllocation {
    pub(crate) component_id: ComponentId,
    pub(crate) stack_index: usize,
    pub(crate) kind: SurfaceKind,
    pub(crate) lane: SurfaceLane,
    pub(crate) reserved_top: usize,
    pub(crate) reserved_bottom: usize,
    pub(crate) allocated_width: usize,
    pub(crate) allocated_rows: usize,
}

/// Allocate per-surface viewport budgets from measured constraints.
pub(crate) fn allocate_surface_budgets(
    measurements: &[SurfaceMeasurement],
    terminal_cols: usize,
    terminal_rows: usize,
) -> Vec<SurfaceAllocation> {
    let terminal_cols = terminal_cols.max(1);
    let terminal_rows = terminal_rows.max(1);

    let mut reserved_top = 0usize;
    let mut reserved_bottom = 0usize;

    measurements
        .iter()
        .map(|measurement| {
            let reserved_top_before = reserved_top;
            let reserved_bottom_before = reserved_bottom;
            let available_rows = terminal_rows
                .saturating_sub(reserved_top_before.saturating_add(reserved_bottom_before));

            let preferred_rows = measurement.preferred_max_height.unwrap_or(available_rows);
            let allocated_rows = preferred_rows.min(available_rows);

            let allocation = SurfaceAllocation {
                component_id: measurement.component_id,
                stack_index: measurement.stack_index,
                kind: measurement.kind,
                lane: measurement.lane,
                reserved_top: reserved_top_before,
                reserved_bottom: reserved_bottom_before,
                allocated_width: measurement.preferred_width.clamp(1, terminal_cols),
                allocated_rows,
            };

            match measurement.lane {
                SurfaceLane::Top => {
                    reserved_top = reserved_top.saturating_add(allocated_rows).min(terminal_rows);
                }
                SurfaceLane::Bottom => {
                    reserved_bottom = reserved_bottom
                        .saturating_add(allocated_rows)
                        .min(terminal_rows);
                }
                SurfaceLane::Floating => {}
            }

            allocation
        })
        .collect()
}

fn resolve_measurement_size(value: Option<SurfaceSizeValue>, reference: usize) -> Option<usize> {
    match value {
        None => None,
        Some(SurfaceSizeValue::Absolute(value)) => Some(value),
        Some(SurfaceSizeValue::Percent(percent)) => {
            let percent = percent.max(0.0);
            Some(((reference as f32) * (percent / 100.0)).floor() as usize)
        }
    }
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

    pub(crate) fn bring_to_front(&mut self, surface_id: SurfaceId) -> bool {
        let Some(index) = self.index_of(surface_id) else {
            return false;
        };
        if index + 1 == self.entries.len() {
            return false;
        }
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        true
    }

    pub(crate) fn send_to_back(&mut self, surface_id: SurfaceId) -> bool {
        let Some(index) = self.index_of(surface_id) else {
            return false;
        };
        if index == 0 {
            return false;
        }
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
        true
    }

    pub(crate) fn raise(&mut self, surface_id: SurfaceId) -> bool {
        let Some(index) = self.index_of(surface_id) else {
            return false;
        };
        if index + 1 == self.entries.len() {
            return false;
        }
        self.entries.swap(index, index + 1);
        true
    }

    pub(crate) fn lower(&mut self, surface_id: SurfaceId) -> bool {
        let Some(index) = self.index_of(surface_id) else {
            return false;
        };
        if index == 0 {
            return false;
        }
        self.entries.swap(index, index - 1);
        true
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
    options.layout.visibility = visibility;
    options
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_surface_budgets, measure_visible_surfaces, surface_options_from_layout,
        SurfaceAnchor, SurfaceEntry, SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceLane,
        SurfaceLayoutOptions, SurfaceMargin, SurfaceOptions, SurfaceRenderEntry, SurfaceSizeValue,
        SurfaceState, SurfaceVisibility,
    };
    use crate::runtime::component_registry::{ComponentId, ComponentRegistry};
    use crate::Component;

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
    fn measurement_pass_resolves_lane_and_size_inputs_deterministically() {
        let component_ids = build_component_ids(4);
        let entries = vec![
            SurfaceRenderEntry {
                component_id: component_ids[0],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Toast,
                    layout: SurfaceLayoutOptions {
                        width: Some(SurfaceSizeValue::percent(50.0)),
                        max_height: Some(SurfaceSizeValue::percent(40.0)),
                        min_width: Some(12),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[1],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Drawer,
                    layout: SurfaceLayoutOptions {
                        width: Some(SurfaceSizeValue::absolute(999)),
                        max_height: Some(SurfaceSizeValue::absolute(99)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[2],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Corner,
                    layout: SurfaceLayoutOptions {
                        width: Some(SurfaceSizeValue::percent(-10.0)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[3],
                options: None,
            },
        ];

        let baseline = measure_visible_surfaces(&entries, 30, 10);
        let rerun = measure_visible_surfaces(&entries, 30, 10);

        assert_eq!(rerun, baseline);
        assert_eq!(baseline.len(), 4);

        assert_eq!(baseline[0].stack_index, 0);
        assert_eq!(baseline[0].lane, SurfaceLane::Top);
        assert_eq!(baseline[0].preferred_width, 15);
        assert_eq!(baseline[0].preferred_max_height, Some(4));

        assert_eq!(baseline[1].stack_index, 1);
        assert_eq!(baseline[1].lane, SurfaceLane::Bottom);
        assert_eq!(baseline[1].preferred_width, 30);
        assert_eq!(baseline[1].preferred_max_height, Some(10));

        assert_eq!(baseline[2].stack_index, 2);
        assert_eq!(baseline[2].lane, SurfaceLane::Floating);
        assert_eq!(baseline[2].preferred_width, 1);
        assert_eq!(baseline[2].preferred_max_height, None);

        assert_eq!(baseline[3].stack_index, 3);
        assert_eq!(baseline[3].kind, SurfaceKind::Modal);
        assert_eq!(baseline[3].lane, SurfaceLane::Floating);
        assert_eq!(baseline[3].preferred_width, 30);
        assert_eq!(baseline[3].preferred_max_height, None);
    }

    #[test]
    fn allocation_pass_clamps_lane_budgets_under_tiny_terminal_constraints() {
        let component_ids = build_component_ids(3);
        let entries = vec![
            SurfaceRenderEntry {
                component_id: component_ids[0],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Toast,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(8)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[1],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Drawer,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(8)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[2],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Modal,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(8)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
        ];

        let measurements = measure_visible_surfaces(&entries, 20, 3);
        let allocations = allocate_surface_budgets(&measurements, 20, 3);

        assert_eq!(allocations.len(), 3);

        assert_eq!(allocations[0].lane, SurfaceLane::Top);
        assert_eq!(allocations[0].reserved_top, 0);
        assert_eq!(allocations[0].reserved_bottom, 0);
        assert_eq!(allocations[0].allocated_rows, 3);

        assert_eq!(allocations[1].lane, SurfaceLane::Bottom);
        assert_eq!(allocations[1].reserved_top, 3);
        assert_eq!(allocations[1].reserved_bottom, 0);
        assert_eq!(allocations[1].allocated_rows, 0);

        assert_eq!(allocations[2].lane, SurfaceLane::Floating);
        assert_eq!(allocations[2].reserved_top, 3);
        assert_eq!(allocations[2].reserved_bottom, 0);
        assert_eq!(allocations[2].allocated_rows, 0);
    }

    #[test]
    fn allocation_pass_preserves_lane_interaction_order_and_replay_determinism() {
        let component_ids = build_component_ids(4);
        let entries = vec![
            SurfaceRenderEntry {
                component_id: component_ids[0],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Toast,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(2)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[1],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Corner,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(99)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[2],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::Drawer,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(2)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
            SurfaceRenderEntry {
                component_id: component_ids[3],
                options: Some(SurfaceOptions {
                    kind: SurfaceKind::AttachmentRow,
                    layout: SurfaceLayoutOptions {
                        max_height: Some(SurfaceSizeValue::absolute(2)),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
        ];

        let measurements = measure_visible_surfaces(&entries, 30, 4);
        let baseline = allocate_surface_budgets(&measurements, 30, 4);
        let rerun = allocate_surface_budgets(&measurements, 30, 4);

        assert_eq!(rerun, baseline);
        assert_eq!(baseline.len(), 4);

        assert_eq!(baseline[0].lane, SurfaceLane::Top);
        assert_eq!(baseline[0].allocated_rows, 2);

        assert_eq!(baseline[1].lane, SurfaceLane::Floating);
        assert_eq!(baseline[1].reserved_top, 2);
        assert_eq!(baseline[1].reserved_bottom, 0);
        assert_eq!(baseline[1].allocated_rows, 2);

        assert_eq!(baseline[2].lane, SurfaceLane::Bottom);
        assert_eq!(baseline[2].reserved_top, 2);
        assert_eq!(baseline[2].reserved_bottom, 0);
        assert_eq!(baseline[2].allocated_rows, 2);

        assert_eq!(baseline[3].lane, SurfaceLane::Bottom);
        assert_eq!(baseline[3].reserved_top, 2);
        assert_eq!(baseline[3].reserved_bottom, 2);
        assert_eq!(baseline[3].allocated_rows, 0);
    }

    #[test]
    fn lane_reservations_adjust_margins_without_mutating_original_layout() {
        let options = SurfaceOptions {
            layout: SurfaceLayoutOptions {
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

        assert_eq!(options.layout.margin, Some(SurfaceMargin::uniform(1)));
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

        let layout_render = crate::render::surface::SurfaceOptions::from(&layout);
        let expected =
            crate::render::surface::resolve_surface_layout(Some(&layout_render), 9, 120, 40);

        let surface_options = surface_options_from_layout(layout);
        let layout_options = surface_options.with_lane_reservations(0, 0);
        let surface_render = crate::render::surface::SurfaceOptions::from(&layout_options);
        let actual =
            crate::render::surface::resolve_surface_layout(Some(&surface_render), 9, 120, 40);

        assert_eq!(actual, expected);
        let round_trip_layout = SurfaceLayoutOptions::from(surface_options);
        assert_eq!(round_trip_layout, layout);
    }

    #[derive(Default)]
    struct TestComponent;

    impl Component for TestComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }

    fn build_component_ids(count: usize) -> Vec<ComponentId> {
        let mut registry = ComponentRegistry::new();
        (0..count)
            .map(|_| registry.register_boxed(Box::new(TestComponent)))
            .collect()
    }

    fn surface_entry(
        surface_id: SurfaceId,
        component_id: ComponentId,
        hidden: bool,
        input_policy: SurfaceInputPolicy,
    ) -> SurfaceEntry {
        SurfaceEntry {
            id: surface_id,
            component_id,
            options: Some(SurfaceOptions {
                input_policy,
                ..Default::default()
            }),
            pre_focus: None,
            hidden,
        }
    }

    fn surface_order(state: &SurfaceState) -> Vec<u64> {
        state.entries.iter().map(|entry| entry.id.raw()).collect()
    }

    #[test]
    fn surface_state_reorder_primitives_follow_noop_and_move_contracts() {
        let components = build_component_ids(3);
        let mut state = SurfaceState {
            entries: vec![
                surface_entry(
                    SurfaceId::from_raw(10),
                    components[0],
                    false,
                    SurfaceInputPolicy::Capture,
                ),
                surface_entry(
                    SurfaceId::from_raw(11),
                    components[1],
                    false,
                    SurfaceInputPolicy::Capture,
                ),
                surface_entry(
                    SurfaceId::from_raw(12),
                    components[2],
                    false,
                    SurfaceInputPolicy::Capture,
                ),
            ],
        };

        assert!(!state.bring_to_front(SurfaceId::from_raw(12)));
        assert!(!state.send_to_back(SurfaceId::from_raw(10)));
        assert!(!state.raise(SurfaceId::from_raw(12)));
        assert!(!state.lower(SurfaceId::from_raw(10)));
        assert!(!state.bring_to_front(SurfaceId::from_raw(99)));

        assert!(state.raise(SurfaceId::from_raw(11)));
        assert_eq!(surface_order(&state), vec![10, 12, 11]);

        assert!(state.lower(SurfaceId::from_raw(12)));
        assert_eq!(surface_order(&state), vec![12, 10, 11]);

        assert!(state.bring_to_front(SurfaceId::from_raw(12)));
        assert_eq!(surface_order(&state), vec![10, 11, 12]);

        assert!(state.send_to_back(SurfaceId::from_raw(11)));
        assert_eq!(surface_order(&state), vec![11, 10, 12]);
    }

    #[test]
    fn hidden_surfaces_can_reorder_without_changing_visible_capture_winner() {
        let components = build_component_ids(3);
        let mut state = SurfaceState {
            entries: vec![
                surface_entry(
                    SurfaceId::from_raw(20),
                    components[0],
                    false,
                    SurfaceInputPolicy::Capture,
                ),
                surface_entry(
                    SurfaceId::from_raw(21),
                    components[1],
                    true,
                    SurfaceInputPolicy::Capture,
                ),
                surface_entry(
                    SurfaceId::from_raw(22),
                    components[2],
                    false,
                    SurfaceInputPolicy::Capture,
                ),
            ],
        };

        let winner_before = state
            .topmost_visible_component(80, 24, true)
            .expect("visible capture winner before reorder");
        assert_eq!(winner_before, components[2]);

        assert!(state.bring_to_front(SurfaceId::from_raw(20)));
        assert_eq!(surface_order(&state), vec![21, 22, 20]);
        let winner_after_front = state
            .topmost_visible_component(80, 24, true)
            .expect("visible capture winner after bring-to-front");
        assert_eq!(winner_after_front, components[0]);

        assert!(state.bring_to_front(SurfaceId::from_raw(21)));
        assert_eq!(surface_order(&state), vec![22, 20, 21]);
        let winner_after_hidden_front = state
            .topmost_visible_component(80, 24, true)
            .expect("visible capture winner after hidden bring-to-front");
        assert_eq!(winner_after_hidden_front, components[0]);

        assert!(state.lower(SurfaceId::from_raw(20)));
        assert_eq!(surface_order(&state), vec![20, 22, 21]);
        let winner_after_lower = state
            .topmost_visible_component(80, 24, true)
            .expect("visible capture winner after lower");
        assert_eq!(winner_after_lower, components[2]);
    }
}
