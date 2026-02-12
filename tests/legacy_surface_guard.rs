use std::path::Path;

fn read_repo_file(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn assert_absent(content: &str, needle: &str, path: &str) {
    assert!(
        !content.contains(needle),
        "legacy symbol `{needle}` must not appear in {path}"
    );
}

fn assert_present(content: &str, needle: &str, path: &str) {
    assert!(
        content.contains(needle),
        "expected canonical symbol `{needle}` in {path}"
    );
}

#[test]
fn crate_root_exports_surface_api_not_legacy_overlay_api() {
    let content = read_repo_file("src/lib.rs");

    for legacy in [
        "OverlayHandle",
        "OverlayId",
        "OverlayOptions",
        "show_overlay(",
    ] {
        assert_absent(&content, legacy, "src/lib.rs");
    }

    for canonical in ["SurfaceHandle", "SurfaceId", "SurfaceOptions"] {
        assert_present(&content, canonical, "src/lib.rs");
    }
}

#[test]
fn runtime_module_exports_surface_types_only() {
    let content = read_repo_file("src/runtime/mod.rs");

    for legacy in [
        "OverlayHandle",
        "OverlayId",
        "OverlayOptions",
        "show_overlay(",
    ] {
        assert_absent(&content, legacy, "src/runtime/mod.rs");
    }

    for canonical in ["SurfaceHandle", "SurfaceId", "SurfaceOptions"] {
        assert_present(&content, canonical, "src/runtime/mod.rs");
    }
}

#[test]
fn runtime_does_not_reintroduce_public_overlay_entrypoints() {
    let content = read_repo_file("src/runtime/tui.rs");

    for legacy in [
        "pub fn show_overlay",
        "pub fn hide_overlay",
        "pub struct OverlayHandle",
        "pub struct OverlayId",
        "pub type OverlayHandle",
        "pub type OverlayId",
        "OverlayOptions",
    ] {
        assert_absent(&content, legacy, "src/runtime/tui.rs");
    }

    for canonical in [
        "pub fn show_surface",
        "pub fn hide_surface",
        "pub struct SurfaceHandle",
    ] {
        assert_present(&content, canonical, "src/runtime/tui.rs");
    }
}

#[test]
fn runtime_overlay_shim_module_is_removed() {
    let overlay_shim = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/overlay.rs");
    assert!(
        !overlay_shim.exists(),
        "legacy runtime overlay shim must remain removed: {}",
        overlay_shim.display()
    );
}

#[test]
fn render_module_uses_surface_compositor_names_only() {
    let mod_content = read_repo_file("src/render/mod.rs");
    assert_absent(&mod_content, "pub mod overlay;", "src/render/mod.rs");
    assert_present(&mod_content, "pub mod surface;", "src/render/mod.rs");

    let surface_content = read_repo_file("src/render/surface.rs");
    for legacy in [
        "OverlayAnchor",
        "OverlayOptions",
        "resolve_overlay_layout",
        "composite_overlays",
    ] {
        assert_absent(&surface_content, legacy, "src/render/surface.rs");
    }

    for canonical in [
        "SurfaceAnchor",
        "SurfaceOptions",
        "resolve_surface_layout",
        "composite_surfaces",
    ] {
        assert_present(&surface_content, canonical, "src/render/surface.rs");
    }
}
