# Roadmap — Rust pi-tui Port

This roadmap breaks the port into concrete milestones with per‑phase checklists and tests, aligned to the parity document (`pi-tui-rust-architecture.md`) and the design principles (correctness, deterministic output, inline‑first, layered, RAII teardown).

---

## Phase 0 — Repo scaffolding & invariants
**Goal:** structure + guardrails aligned with design philosophy.

**Checklist**
- [ ] Create module layout (`core/`, `render/`, `runtime/`, `platform/`, `widgets/`)
- [ ] Define “single output gate” rule (renderer is only writer)
- [ ] Add env config plumbing:
  - `PI_HARDWARE_CURSOR`, `PI_CLEAR_ON_SHRINK`, `PI_TUI_WRITE_LOG`, `PI_TUI_DEBUG`, `PI_DEBUG_REDRAW`
- [ ] Add basic logging helpers (render/debug logs)

**Tests**
- [ ] Unit: config/env parsing returns correct defaults
- [ ] Unit: module layering lint (optional)

---

## Phase 1 — Terminal + RAII teardown (correctness baseline)
**Goal:** terminal state safety and deterministic lifecycle.

**Checklist**
- [ ] `Terminal` trait with full parity API
- [ ] `ProcessTerminal` implementation:
  - raw mode enter/restore
  - bracketed paste on/off
  - Kitty query (`CSI ? u`) + enable/disable (`>7u` / `<u`)
  - resize handling + SIGWINCH refresh
  - `stdin.pause()` on stop
  - `drainInput()` with idle/max window
  - optional write log (`PI_TUI_WRITE_LOG`)
- [ ] `TerminalGuard` RAII wrapper
- [ ] panic + signal cleanup hooks

**Tests**
- [ ] Integration: start/stop restores raw mode and cursor
- [ ] Integration: bracketed paste toggled on/off
- [ ] Integration: `drainInput()` returns within max window
- [ ] Integration: `stdin.pause()` called before raw mode exit (mockable)

---

## Phase 2 — Input pipeline & key parsing
**Goal:** exact parity input semantics.

**Checklist**
- [ ] `StdinBuffer`:
  - CSI/OSC/DCS/APC/SS3 + old mouse
  - flush after 10ms idle
  - high‑bit byte conversion
  - bracketed paste rewrap
- [ ] `KeyId`, `matchesKey()`, `parseKey()`:
  - Kitty protocol parsing
  - modifyOtherKeys fallback
  - base‑layout fallback rule for non‑Latin keys only
  - `shift+enter`/`alt+enter` ambiguity rules
  - `isKeyRelease()` ignores bracketed paste markers
  - key‑release filtering depends on `wantsKeyRelease`

**Tests**
- [ ] Unit: StdinBuffer splits partial sequences correctly
- [ ] Unit: 10ms timeout flush yields buffered data
- [ ] Unit: bracketed paste rewrap behavior
- [ ] Unit: kitty vs legacy key mappings (shift+enter, alt+enter)
- [ ] Unit: base‑layout fallback only for non‑Latin keys
- [ ] Unit: modifyOtherKeys fallback works when kitty inactive

---

## Phase 3 — ANSI/Unicode width + slicing engine
**Goal:** correctness for rendering and compositing.

**Checklist**
- [ ] `visible_width()` using graphemes + emoji width rules
- [ ] `extract_ansi_code()` for CSI/OSC/APC
- [ ] `slice_by_column`, `slice_with_width`, `extract_segments`
- [ ] `wrap_text_with_ansi`
- [ ] underline bleed fix (`CSI 24 m`)

**Tests**
- [ ] Unit: ANSI codes ignored in width
- [ ] Unit: OSC‑8 hyperlinks ignored
- [ ] Unit: RGI emoji width = 2
- [ ] Unit: strict slicing drops boundary wide chars
- [ ] Unit: `extract_segments()` preserves style inheritance
- [ ] Unit: underline bleed reset is inserted

---

## Phase 4 — Core renderer (no overlays/IME/images yet)
**Goal:** deterministic diff renderer with sync output.

**Checklist**
- [ ] `DiffRenderer` with tracked state:
  - `previous_lines`, `previous_width`,
  - `max_lines_rendered`, `cursor_row`,
  - `hardware_cursor_row`, `previous_viewport_top`
- [ ] First render (no clear)
- [ ] Width change full clear
- [ ] Diff render path
- [ ] Synchronized output wrapping (`CSI ?2026 h/l`) for full + diff renders
- [ ] `clearOnShrink` full redraw only when shrink + no overlays (`PI_CLEAR_ON_SHRINK`)
- [ ] `SEGMENT_RESET` on non‑image lines
- [ ] Hard width check **only in diff path**
- [ ] `PI_DEBUG_REDRAW` + `PI_TUI_DEBUG` logging

**Tests**
- [ ] Integration: identical render twice → zero output
- [ ] Integration: width change causes full clear
- [ ] Integration: diff path updates only changed lines
- [ ] Integration: overflow line crashes only on diff path
- [ ] Integration: `SEGMENT_RESET` appended to non‑image lines

---

## Phase 5 — IME cursor + focus management
**Goal:** IME‑safe cursor behavior.

**Checklist**
- [ ] `CURSOR_MARKER` extraction before resets
- [ ] Hardware cursor positioning (row delta + column absolute)
- [ ] `Focusable` trait + `set_focus()` behavior
- [ ] `wantsKeyRelease` filtering in input dispatch
- [ ] `onDebug` handler (`shift+ctrl+d`) runs before forwarding input
- [ ] TUI stop cursor placement (move to end + newline)
- [ ] `PI_HARDWARE_CURSOR` toggle

**Tests**
- [ ] Unit: marker removed, cursor position computed
- [ ] Integration: cursor movement commands issued correctly
- [ ] Unit: key release filtered unless `wantsKeyRelease=true`

---

## Phase 6 — Images + cell size query
**Goal:** image lines + cell dimension handling.

**Checklist**
- [ ] `terminal_image` capabilities + `is_image_line`
- [ ] Cell size query (`CSI 16 t`) + response parse
- [ ] `setCellDimensions` + `invalidate()` all components
- [ ] Skip width checks and resets on image lines

**Tests**
- [ ] Unit: parse `CSI 6;h;w t` response
- [ ] Integration: image lines bypass width enforcement
- [ ] Integration: cell size query triggers re‑render

---

## Phase 7 — Overlays + compositing
**Goal:** parity overlay behavior.

**Checklist**
- [ ] Overlay stack with visibility + `setHidden`
- [ ] Focus handling (`preFocus` restore)
- [ ] Layout resolution: anchor / percent / margin / offsets
- [ ] Padding to `max(max_lines_rendered, overlay_bottom)`
- [ ] Composite using `extract_segments()` + `slice_with_width()`
- [ ] Post‑composite width verification

**Tests**
- [ ] Unit: layout resolution for anchors/percent
- [ ] Integration: overlay focus handoff + restoration
- [ ] Integration: overlay visibility callback on resize
- [ ] Integration: compositing preserves styles + width

---

## Phase 8 — Widgets (optional)
**Goal:** higher‑level components.

**Checklist**
- [ ] Minimal `Text`, `Container`, `Spacer`
- [ ] Later: Editor/Markdown/SelectList, etc.

**Tests**
- [ ] Rendering width constraints on built‑ins
- [ ] Input routing for interactive widgets

---

## Phase 9 — Parity regression suite
**Goal:** high‑confidence behavior matches pi‑tui.

**Checklist**
- [ ] Cross‑check render logs vs pi‑tui on identical fixtures
- [ ] Key parsing golden tests (kitty + legacy)
- [ ] Cursor marker + overlay test scenarios

**Tests**
- [ ] Golden fixtures for renderer diffs
- [ ] Replay input streams vs expected key IDs

---

## Phase 10 — Utils parity
**Goal:** finish remaining `utils.ts` parity helpers.

**Checklist**
- [ ] `truncate_to_width`
- [ ] `apply_background_to_line`
- [ ] `is_whitespace_char`
- [ ] `is_punctuation_char`
- [ ] Segmenter helper (if needed for Input/Editor)

**Tests**
- [ ] Unit: truncate/padding/ellipsis cases
- [ ] Unit: whitespace/punctuation classification

---

## Phase 11 — Key helper API
**Goal:** finish key helper surface from `keys.ts`.

**Checklist**
- [ ] `Key` helper builder
- [ ] `KeyEventType`
- [ ] `is_key_repeat`

**Tests**
- [ ] Unit: `Key` builder + `is_key_repeat`

---

## Phase 12 — Editor keybindings
**Goal:** port `keybindings.ts` support.

**Checklist**
- [ ] `EditorAction`, `EditorKeybindingsConfig`
- [ ] `DEFAULT_EDITOR_KEYBINDINGS`
- [ ] `EditorKeybindingsManager` + global get/set

**Tests**
- [ ] Unit: keybindings defaults + overrides

---

## Phase 13 — Fuzzy matching
**Goal:** port `fuzzy.ts` utilities.

**Checklist**
- [ ] `FuzzyMatch`
- [ ] `fuzzy_match`
- [ ] `fuzzy_filter`

**Tests**
- [ ] Unit: scoring + token matching

---

## Phase 14 — Autocomplete providers
**Goal:** port autocomplete provider APIs.

**Checklist**
- [ ] `AutocompleteItem`, `AutocompleteSuggestions`, `AutocompleteProvider`
- [ ] `SlashCommand` support
- [ ] `CombinedAutocompleteProvider` (slash commands, file paths, quoted prefixes)
- [ ] Async suggestions + cancellation hooks (fd integration)

**Tests**
- [ ] Unit: prefix parsing + quoting rules
- [ ] Unit: `apply_completion` behavior
- [ ] Unit: async update/cancel flow (mocked)

---

## Phase 15 — Terminal image helpers: encoding + IDs
**Goal:** implement image encoding and ID management.

**Checklist**
- [ ] `encode_kitty`, `encode_iterm2`
- [ ] `allocate_image_id`
- [ ] `delete_kitty_image`, `delete_all_kitty_images`

**Tests**
- [ ] Unit: encoder output + delete sequences + ID range

---

## Phase 16 — Terminal image helpers: dimensions + render
**Goal:** implement dimension parsing and render helpers.

**Checklist**
- [ ] `get_png_dimensions`, `get_jpeg_dimensions`, `get_gif_dimensions`, `get_webp_dimensions`
- [ ] `get_image_dimensions`
- [ ] `calculate_image_rows`
- [ ] `render_image`
- [ ] `image_fallback`

**Tests**
- [ ] Unit: dimension parsing + row calculation + render output

---

## Phase 17 — Runtime async render handle
**Goal:** enable async components to schedule renders safely.

**Checklist**
- [ ] Thread-safe render request handle (no direct terminal writes)
- [ ] Scheduling hooks for background tasks

**Tests**
- [ ] Integration: render request from background task

---

## Phase 18 — Box + TruncatedText
**Goal:** basic layout components parity.

**Checklist**
- [ ] `Box` (padding/background + caching)
- [ ] `TruncatedText`

**Tests**
- [ ] Unit: Box padding/background width
- [ ] Unit: TruncatedText width + ellipsis

---

## Phase 19 — Input component
**Goal:** single-line input parity.

**Checklist**
- [ ] Cursor movement (grapheme-aware)
- [ ] Word navigation/delete
- [ ] Bracketed paste handling
- [ ] onSubmit/onEscape hooks

**Tests**
- [ ] Integration: editing + cursor movement
- [ ] Integration: paste + delete word

---

## Phase 20 — SelectList component
**Goal:** selection list parity.

**Checklist**
- [ ] Selection + scroll behavior
- [ ] Keybinding navigation
- [ ] Callbacks (`onSelect`, `onCancel`, `onSelectionChange`)

**Tests**
- [ ] Integration: navigation + selection state

---

## Phase 21 — SettingsList component
**Goal:** settings list parity.

**Checklist**
- [ ] Search input integration
- [ ] Fuzzy filter + selection
- [ ] Submenu handling + value cycling

**Tests**
- [ ] Integration: search + navigation + selection

---

## Phase 22 — Markdown component
**Goal:** Markdown rendering parity.

**Checklist**
- [ ] Theme + default styles
- [ ] ANSI styling + wrapping
- [ ] Cache behavior

**Tests**
- [ ] Integration: Markdown rendering fixtures

---

## Phase 23 — Loader + CancellableLoader
**Goal:** timer-driven status components.

**Checklist**
- [ ] Loader spinner frames
- [ ] Cancellable loader interaction
- [ ] Render scheduling on ticks

**Tests**
- [ ] Integration: tick updates + cancel hook

---

## Phase 24 — Image component
**Goal:** image component parity using terminal-image helpers.

**Checklist**
- [ ] Kitty/iTerm2 rendering
- [ ] Fallback rendering
- [ ] Image ID reuse

**Tests**
- [ ] Integration: image fallback + sequence output

---

## Phase 25 — Editor core: layout + navigation
**Goal:** multi-line editor foundation without autocomplete.

**Checklist**
- [ ] `EditorComponent` trait parity
- [ ] Word-wrapped layout + viewport scrolling
- [ ] Cursor movement + selection boundaries

**Tests**
- [ ] Integration: navigation + layout + selection

---

## Phase 26 — Editor core: editing + undo
**Goal:** editing operations and history.

**Checklist**
- [ ] Insert/delete/kill/yank
- [ ] Undo/redo

**Tests**
- [ ] Integration: editing + undo/redo + kill/yank

---

## Phase 27 — Editor autocomplete integration
**Goal:** wire autocomplete providers into the editor.

**Checklist**
- [ ] Autocomplete trigger + selection UI
- [ ] Async suggestion updates
- [ ] Apply completion semantics

**Tests**
- [ ] Integration: autocomplete flow + applyCompletion

---

## Phase 28 — Public API parity sweep
**Goal:** final exports aligned to `packages/tui/src/index.ts`.

**Checklist**
- [ ] `lib.rs` re-exports match TS index
- [ ] Final parity spot-checks

**Tests**
- [ ] Compile-time export smoke checks

---

## Phase 29 — Parity closure (pending gaps)
**Goal:** close remaining parity gaps from earlier phases and document known divergences.

**Checklist**
- [x] Implement `EnvConfig` parsing + tests (`PI_HARDWARE_CURSOR`, `PI_CLEAR_ON_SHRINK`, `PI_TUI_WRITE_LOG`, `PI_TUI_DEBUG`, `PI_DEBUG_REDRAW`)
- [x] Align Input prompt default with pi-tui (`"> "`), keep configurable
- [x] Align `allocate_image_id()` to random ID semantics
- [x] Enforce `max_height_cells` in image rendering (intentional divergence: TS currently ignores `maxHeightCells`)
- [x] Add `timeout` alias in `StdinBufferOptions`
- [x] Add Editor getters for padding + autocomplete max visible
- [x] Resolve `render/frame.rs` (implement or remove)
- [x] Document open-ended divergences in `pi-tui-rust-architecture.md` (width logic, markdown parsing)

**Tests**
- [x] Unit: config/env parsing defaults + flags
- [x] Unit: StdinBufferOptions timeout alias (if added)
- [x] Unit: Editor getters (if added)
- [x] Unit: Input render includes default prompt
- [x] Unit: `render_image` respects `max_height_cells` (rows <= max height; Kitty c/r params updated)
