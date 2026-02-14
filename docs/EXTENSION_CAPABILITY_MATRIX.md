# Extension Capability Matrix (TS vs Rust)

This matrix maps extension-relevant capabilities to the **actual exports** from:
- TypeScript: `https://github.com/badlogic/pi-mono/tree/main/packages/tui/src/index.ts`
- Rust: `https://github.com/Gurpartap/tape_tui/tree/main/src/lib.rs` (plus public modules declared there)

The goal is a crisp, auditable map of what extensions can do or access today.

| Extension Action | TS Support (exports) | Rust Support (exports) | Notes |
|---|---|---|---|
| Create a runtime instance | `TUI` class | `TUI<T>` type alias (`TuiRuntime<T>`) | Same role, different naming. |
| Schedule a render from the UI thread | `TUI.requestRender()` | `TuiRuntime::request_render()` | TS coalesces via `process.nextTick`; Rust renders per event loop cycle. |
| Schedule a render from another thread/task | No dedicated handle | `RuntimeHandle::dispatch(Command::RequestRender)` | Rust provides a thread-safe handle; TS requires access to `TUI`. |
| Set terminal title | `Terminal.setTitle()` | `TerminalTitleExt::set_title()`, `TuiRuntime::set_title()`, `RuntimeHandle::dispatch(Command::SetTitle(..))` | Rust has runtime-safe and terminal-owner options. |
| Observe runtime diagnostics | No dedicated runtime diagnostic hook | `TuiRuntime::set_on_diagnostic(..)` | Rust exposes structured runtime warnings/errors for invalid IDs and custom command failures. |
| Define a custom component | `Component` interface | `Component` trait | Both require `render` and input handling hooks. |
| Focus handling and IME cursor marker | `Focusable`, `CURSOR_MARKER` | `Focusable`, `CURSOR_MARKER` | Equivalent concept. |
| Overlay creation and management (TS API) | `OverlayOptions`, `OverlayHandle`, `TUI.showOverlay()` | Not exported | Rust runtime/public API is surface-only; legacy overlay lifecycle entrypoints are intentionally absent. |
| Surface creation and management | No first-class surface API | `SurfaceOptions`, `SurfaceHandle`, `SurfaceKind`, `SurfaceInputPolicy`, `TuiRuntime::show_surface()` | Rust provides the canonical transient-layer API for host/extension composition. |
| Surface input arbitration semantics | Implicit transient-layer event flow | Runtime-managed capture-first arbitration with internal `Consumed`/`Ignored` bubbling to deterministic fallback targets | Passthrough surfaces do not own input; ignored capture events bubble to pre-focus/focused/root fallback targets. |
| Surface positioning types | `OverlayAnchor`, `OverlayMargin`, `SizeValue` | `SurfaceAnchor`, `SurfaceMargin`, `SurfaceSizeValue` (+ `SurfaceKind` lane defaults) | Rust preserves geometry semantics while using surface-native type names. |
| Widget set (Editor, Input, Markdown, etc.) | `Editor`, `Input`, `Markdown`, `SelectList`, `SettingsList`, `Image`, `Loader`, `CancellableLoader`, `Box`, `Text`, `Spacer`, `TruncatedText` | Same widgets re-exported | Rust also exports editor option types and themes. |
| Autocomplete providers | `AutocompleteItem`, `AutocompleteProvider`, `CombinedAutocompleteProvider`, `SlashCommand` | Same re-exports | Parity in provider surface. |
| Keybindings manager | `EditorKeybindingsManager`, `getEditorKeybindings`, `setEditorKeybindings`, `DEFAULT_EDITOR_KEYBINDINGS` | `EditorKeybindingsManager`, `EditorKeybindingsHandle`, `default_editor_keybindings_handle`, `DEFAULT_EDITOR_KEYBINDINGS` | Rust exposes a handle-based API; naming differs. |
| Fuzzy matching | `fuzzyMatch`, `fuzzyFilter`, `FuzzyMatch` | `fuzzy_match`, `fuzzy_filter`, `FuzzyMatch` | Naming differences only. |
| Keyboard parsing helpers | `Key`, `KeyId`, `KeyEventType`, `matchesKey`, `parseKey`, `isKeyRelease`, `isKeyRepeat`, `setKittyProtocolActive`, `isKittyProtocolActive` | `Key`, `KeyId`, `KeyEventType`, `matches_key`, `parse_key`, `is_key_release`, `is_key_repeat` | Rust does not export Kitty protocol toggles as public helpers. |
| Input buffering | `StdinBuffer`, `StdinBufferOptions`, `StdinBufferEventMap` | Same exports | Direct parity. |
| Terminal interface and implementation | `Terminal`, `ProcessTerminal` | `Terminal`, `ProcessTerminal` | TS terminal is wider (cursor/clear helpers); Rust terminal is minimal. |
| Terminal image support | `detectCapabilities`, `getCapabilities`, `encodeKitty`, `encodeITerm2`, `renderImage`, `calculateImageRows`, `allocateImageId`, `deleteKittyImage`, `deleteAllKittyImages`, `getCellDimensions`, `setCellDimensions` | Same exports plus `reset_capabilities_cache` and type re-exports | Parity with minor naming differences. |
| Text/ANSI utilities | `visibleWidth`, `wrapTextWithAnsi`, `truncateToWidth` | `visible_width`, `wrap_text_with_ansi`, `truncate_to_width` | Naming differences only. |
| Debug hook | `TUI.onDebug` | `TuiRuntime::set_on_debug()` | Same capability; Rust uses setter. |
| Direct terminal writes | `Terminal.write()` | `Terminal::write()` | Rust architecture prefers `OutputGate` usage; TS uses direct writes in runtime. |
| Explicit raw terminal escape hatch | No dedicated guarded API | `TuiRuntime::terminal_guard_unsafe().write_raw(..)` behind `unsafe-terminal-access` feature | Intentionally bypasses output-gate guarantee; write-only scope; drop requests full redraw + render on next tick. |
| Output gate and terminal command types | Not exported | `core::output::{OutputGate, TerminalCmd}` via `pub mod core` | Rust exposes lower-level primitives to extensions. |
| Crash/panic cleanup hooks | Not exported | `platform::process_terminal` types via `pub mod platform` | Rust exposes internal safety plumbing as public modules. |
| Internal modules surface | Curated `index.ts` only | `pub mod core`, `pub mod render`, `pub mod runtime`, `pub mod platform`, `pub mod widgets` | Rust exposes internal modules, increasing power and surface area. |

## Notes
- This matrix reflects **exported** APIs only. It does not imply stability or recommended usage.
- Rust’s public module exports mean extensions can reach deeper internals than in the TS reference.
- In safe/default builds, terminal output ordering is guaranteed through `OutputGate::flush()` for runtime rendering/command flow. The `unsafe-terminal-access` feature is an opt-in exception for advanced Rust extensions only.
- Runtime changes affecting output behavior should be validated in both build modes: `cargo test` and `cargo test --features unsafe-terminal-access`.
- Surface input routing uses explicit policies (`Capture`, `Passthrough`) with deterministic capture-first arbitration; ignored capture events bubble via internal `Consumed`/`Ignored` semantics to pre-focus/focused/root fallback targets.
