//! Legacy overlay layout aliases retained for internal migration notes.
//!
//! The runtime no longer exposes overlay lifecycle APIs; surface types are canonical.

/// Legacy alias for [`crate::runtime::surface::SurfaceSizeValue`].
pub type SizeValue = crate::runtime::surface::SurfaceSizeValue;
/// Legacy alias for [`crate::runtime::surface::SurfaceAnchor`].
pub type OverlayAnchor = crate::runtime::surface::SurfaceAnchor;
/// Legacy alias for [`crate::runtime::surface::SurfaceMargin`].
pub type OverlayMargin = crate::runtime::surface::SurfaceMargin;
/// Legacy alias for [`crate::runtime::surface::SurfaceVisibility`].
pub type OverlayVisibility = crate::runtime::surface::SurfaceVisibility;
/// Legacy alias for [`crate::runtime::surface::SurfaceLayoutOptions`].
pub type OverlayOptions = crate::runtime::surface::SurfaceLayoutOptions;

/// Legacy visibility helper kept as a thin wrapper.
pub fn is_overlay_visible(options: Option<&OverlayOptions>, columns: usize, rows: usize) -> bool {
    options.map_or(true, |options| options.is_visible(columns, rows))
}
