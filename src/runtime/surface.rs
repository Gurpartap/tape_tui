//! Runtime-owned surface primitives.
//!
//! Surfaces are transient UI layers composed above the scrolling root transcript.
//! They provide deterministic lifecycle, visibility, input policy, and lane semantics
//! while preserving inline-first rendering behavior.

use crate::runtime::component_registry::ComponentId;
use crate::runtime::overlay::{
    OverlayAnchor, OverlayId, OverlayMargin, OverlayOptions, OverlayVisibility, SizeValue,
};

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

impl From<OverlayId> for SurfaceId {
    fn from(value: OverlayId) -> Self {
        Self::from_raw(value.raw())
    }
}

impl From<SurfaceId> for OverlayId {
    fn from(value: SurfaceId) -> Self {
        OverlayId::from_raw(value.raw())
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

/// Surface options composed from legacy overlay geometry plus surface semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceOptions {
    /// Layout and visibility controls shared with overlays.
    pub overlay: OverlayOptions,
    /// Surface class used for lane defaults and reservation behavior.
    pub kind: SurfaceKind,
    /// Input routing behavior.
    pub input_policy: SurfaceInputPolicy,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            overlay: OverlayOptions::default(),
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

    /// Return overlay layout options adjusted for lane reservations and surface defaults.
    ///
    /// Reasoning:
    /// - We keep geometry resolution in one place so compositing and input hit-targeting
    ///   are derived from the same options.
    /// - Lane reservations are additive margins; they do not mutate transcript ordering.
    pub fn with_lane_reservations(
        &self,
        reserved_top: usize,
        reserved_bottom: usize,
    ) -> OverlayOptions {
        let mut overlay = self.overlay;

        if reserved_top > 0 || reserved_bottom > 0 {
            let mut margin = overlay.margin.unwrap_or(OverlayMargin {
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
            overlay.margin = Some(margin);
        }

        match self.kind {
            SurfaceKind::Modal => {}
            SurfaceKind::Drawer => {
                if overlay.anchor.is_none() && overlay.row.is_none() {
                    overlay.anchor = Some(OverlayAnchor::BottomCenter);
                }
            }
            SurfaceKind::Corner => {
                if overlay.anchor.is_none() && overlay.row.is_none() && overlay.col.is_none() {
                    overlay.anchor = Some(OverlayAnchor::BottomRight);
                }
            }
            SurfaceKind::Toast => {
                if overlay.row.is_none() {
                    overlay.row = Some(SizeValue::absolute(0));
                }
                if overlay.anchor.is_none() && overlay.col.is_none() {
                    overlay.anchor = Some(OverlayAnchor::TopRight);
                }
            }
            SurfaceKind::AttachmentRow => {
                if overlay.anchor.is_none() && overlay.row.is_none() {
                    overlay.anchor = Some(OverlayAnchor::BottomLeft);
                }
            }
        }

        overlay
    }
}

impl From<OverlayOptions> for SurfaceOptions {
    fn from(overlay: OverlayOptions) -> Self {
        Self {
            overlay,
            kind: SurfaceKind::Modal,
            input_policy: SurfaceInputPolicy::Capture,
        }
    }
}

impl From<&OverlayOptions> for SurfaceOptions {
    fn from(overlay: &OverlayOptions) -> Self {
        Self::from(*overlay)
    }
}

impl From<SurfaceOptions> for OverlayOptions {
    fn from(options: SurfaceOptions) -> Self {
        options.overlay
    }
}

impl From<&SurfaceOptions> for OverlayOptions {
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

/// Convenience helper to build default overlay-compatible surface options.
pub fn overlay_compatible_options(overlay: OverlayOptions) -> SurfaceOptions {
    SurfaceOptions::from(overlay)
}

/// Convenience helper to create surface options from visibility-only defaults.
pub fn visibility_only_surface_options(visibility: OverlayVisibility) -> SurfaceOptions {
    let mut options = SurfaceOptions::default();
    options.overlay.visibility = visibility;
    options
}
