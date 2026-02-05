# Rust port of pi TUI — architecture notes (parity with `pi-mono/packages/tui`)

This is a parity‑focused extraction of pi‑tui’s core behaviors and a Rust architecture that mirrors them exactly while honoring your design principles (correctness, deterministic output, inline‑first, layered, RAII teardown).

All details below are aligned with:
- `packages/tui/src/tui.ts`
- `packages/tui/src/terminal.ts`
- `packages/tui/src/stdin-buffer.ts`
- `packages/tui/src/keys.ts`
- `packages/tui/src/utils.ts`

---

## Minimum viable core (parity‑critical behaviors)

### 1) Terminal abstraction + raw mode lifecycle
**Terminal API must include:**
- `start(onInput, onResize)`
- `stop()`
- `drainInput(maxMs, idleMs)`
- `write(data)`
- `columns`, `rows`, `kittyProtocolActive`
- `moveBy(lines)`
- `hideCursor()`, `showCursor()`
- `clearLine()`, `clearFromCursor()`, `clearScreen()`
- `setTitle(title)`

**Process terminal behavior (parity):**
- On `start()`:
  - Store prior raw state; enable raw mode.
  - `stdin.setEncoding("utf8")`, `stdin.resume()`.
  - Enable bracketed paste: `CSI ?2004 h`.
  - Attach resize handler; trigger `SIGWINCH` to refresh stale dimensions on Unix.
  - Query Kitty protocol: `CSI ? u` and detect response via `StdinBuffer`.
- On Kitty response (`CSI ?<flags>u`):
  - Set global Kitty active; enable protocol with `CSI >7u`.
- `write()` optionally logs to `PI_TUI_WRITE_LOG`.
- On `stop()`:
  - Disable bracketed paste: `CSI ?2004 l`.
  - Disable Kitty protocol if active: `CSI <u`.
  - Remove input/resize handlers.
  - `stdin.pause()` **before** leaving raw mode to avoid Ctrl‑D leakage.
  - Restore original raw mode.
- `drainInput()`:
  - Disable Kitty protocol first to avoid new release events.
  - Temporarily detach input handler and wait for idle window to prevent key‑release leakage (slow SSH).

### 2) Component interface (input filtering parity)
- `render(width) -> Vec<String>` (pure, deterministic).
- `handleInput(data)`.
- `invalidate()`.
- `wantsKeyRelease: bool` (default false). If false, key‑release events are filtered out before dispatch.

### 3) Input pipeline (exact behavior)
- **StdinBuffer** splits partial escape sequences and emits complete sequences; recognizes CSI/OSC/DCS/APC/SS3 and old‑style mouse (`ESC[M` + 3 bytes).
- Incomplete sequences are flushed after a timeout (default 10ms).
- Converts single high‑bit bytes to `ESC + (byte-128)` for legacy compatibility.
- Bracketed paste is **rewrapped** to `\x1b[200~ ... \x1b[201~`.
- Kitty protocol:
  - Query with `\x1b[?u`.
  - Enable with `\x1b[>7u`.
  - Disable with `\x1b[<u`.
- `matchesKey()` and `parseKey()`:
  - Respect Kitty protocol when active.
  - Fall back to legacy sequences when inactive.
  - Use xterm modifyOtherKeys fallback (`CSI 27;...~`) when Kitty isn’t active.
  - Kitty base layout key fallback only when the codepoint is **not** a Latin letter or known symbol (prevents Dvorak/Colemak mismatches).
  - Handle special cases like `shift+enter` vs `alt+enter` depending on protocol state.
- TUI input handling:
  - Buffer input if cell‑size query is pending; parse response and only forward remaining data.
  - `shift+ctrl+d` triggers `onDebug` before forwarding to focused component.
  - Filter `isKeyRelease()` unless `wantsKeyRelease` is true.
  - Always `requestRender()` after handling input.

### 4) Renderer core (deterministic diff + synchronized output)
- Use synchronized output: `CSI ?2026 h/l`.
- **First render** preserves scrollback (no clear).
- **Width change** triggers full clear (`CSI 3J`, `CSI 2J`, `CSI H`) + full render.
- `clearOnShrink` **only** when no overlays.
- Track: `previous_lines`, `previous_width`, `max_lines_rendered`, `cursor_row` (logical end of content), `hardware_cursor_row` (actual terminal cursor), `previous_viewport_top`.
- Append `SEGMENT_RESET = "\x1b[0m\x1b]8;;\x07"` to every **non‑image** line.
- Skip width checks and reset appends for `isImageLine()`.
- Hard error if any non‑image line exceeds terminal width **during diff render of changed lines** (crash log + teardown); full render does not validate widths (parity).
- `PI_TUI_DEBUG` and `PI_DEBUG_REDRAW` write debug logs for render decisions.

### 5) IME cursor marker (APC) + hardware cursor
- `CURSOR_MARKER = "\x1b_pi:c\x07"`.
- Extract marker **before** line resets; compute column via `visibleWidth()`.
- Position hardware cursor (absolute column via `CSI n G`, relative row via `CSI n A/B`).
- Hardware cursor visibility toggled by `PI_HARDWARE_CURSOR`.

### 6) Cell size query for images
- If images supported (`getCapabilities().images`): send `CSI 16 t`.
- Parse response: `CSI 6 ; height ; width t`.
- `setCellDimensions({widthPx,heightPx})`, invalidate components, request render.
- Handle partial responses (buffer until complete).

### 7) Teardown (zero‑surprise, crash‑safe)
- Disable bracketed paste (`CSI ?2004 l`).
- Disable Kitty protocol (`CSI <u`) if active.
- `stdin.pause()` before leaving raw mode.
- `drainInput()` used on exit to prevent key‑release leaks.
- Restore raw mode and cursor visibility.
- Provide RAII guard + panic/signal cleanup.

### 8) Env toggles (parity)
- `PI_HARDWARE_CURSOR`
- `PI_CLEAR_ON_SHRINK`
- `PI_TUI_WRITE_LOG`
- `PI_TUI_DEBUG`
- `PI_DEBUG_REDRAW`

---

## Rust module layout (core → render → runtime → widgets)

```
src/
  lib.rs
  core/
    mod.rs
    terminal.rs         // Terminal trait + capability types
    component.rs        // Component + Focusable traits (wantsKeyRelease)
    input.rs            // KeyId, KeyEvent, matchesKey/parseKey
    terminal_image.rs   // capabilities, isImageLine, cell dimensions
  render/
    mod.rs
    renderer.rs         // DiffRenderer (synchronized output + diff)
    frame.rs            // Frame = Vec<Line>
    ansi.rs             // ANSI scanner + style tracker
    width.rs            // grapheme width + visibleWidth
    slice.rs            // slice_by_column, extract_segments
    overlay.rs          // compositing helpers (staged)
  runtime/
    mod.rs
    tui.rs              // TuiRuntime: input loop + render scheduling
    focus.rs            // focus management + overlays
    ime.rs              // cursor marker extraction + hardware cursor
  platform/
    mod.rs
    process_terminal.rs // raw mode + tty IO + kitty query/enable
    stdin_buffer.rs     // escape-sequence buffering + paste logic
  widgets/              // optional components
```

Layering:
- `core` has no dependencies on `render`/`runtime`.
- `render` depends on `core` only.
- `runtime` depends on `core` + `render`.
- `widgets` depends on `core` only.

---

## Key traits/types (Rust sketches)

```rust
// core/terminal.rs
pub trait Terminal {
    fn start(&mut self,
             on_input: Box<dyn FnMut(String) + Send>,
             on_resize: Box<dyn FnMut() + Send>);
    fn stop(&mut self);
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64);

    fn write(&mut self, data: &str);

    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
    fn kitty_protocol_active(&self) -> bool;

    fn move_by(&mut self, lines: i32);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);

    fn clear_line(&mut self);
    fn clear_from_cursor(&mut self);
    fn clear_screen(&mut self);
    fn set_title(&mut self, title: &str);
}

// core/component.rs
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, _data: &str) {}
    fn invalidate(&mut self) {}
    fn wants_key_release(&self) -> bool { false }
}

pub trait Focusable {
    fn set_focused(&mut self, focused: bool);
    fn is_focused(&self) -> bool;
}

// render/renderer.rs
pub struct DiffRenderer {
    previous_lines: Vec<String>,
    previous_width: usize,
    max_lines_rendered: usize,
    hardware_cursor_row: usize,
    previous_viewport_top: usize,
}

impl DiffRenderer {
    pub fn render(
        &mut self,
        term: &mut dyn Terminal,
        lines: Vec<String>,
        cursor: Option<CursorPos>, // IME cursor position if any
        is_image_line: fn(&str) -> bool,
    );
}

// runtime/tui.rs
pub struct TuiRuntime<T: Terminal> {
    terminal: T,
    root: Box<dyn Component>,
    renderer: DiffRenderer,
    focused: Option<usize>,
    on_debug: Option<Box<dyn FnMut()>>,
    clear_on_shrink: bool, // PI_CLEAR_ON_SHRINK
    show_hardware_cursor: bool, // PI_HARDWARE_CURSOR
}
```

---

## Render pipeline (step‑by‑step, mirrors pi‑tui)

### A) First render (no clear, preserve scrollback)
1. `lines = root.render(width)`
2. Composite overlays (if any) **before** diffing
3. Extract IME cursor marker from `lines` (before resets)
4. Append `SEGMENT_RESET` to every **non‑image** line
5. Full render (no clear):
   - `CSI ?2026 h` (synchronized output)
   - print all lines with `\r\n`
   - `CSI ?2026 l`
6. Update `previous_lines`, `previous_width`, `max_lines_rendered`, `previous_viewport_top`
7. Position hardware cursor (IME)

### B) Width change (full clear)
- If `prev_width != width`, **full clear + full render**:
  - `CSI 3J` (scrollback), `CSI 2J` (screen), `CSI H` (home)
  - then print all lines

### C) Diff render (steady state)
1. Compute `first_changed`, `last_changed` over `max(prev_lines, new_lines)`
2. If no changes: only update hardware cursor (IME)
3. If change is above previous viewport: full render (inline‑first correctness)
4. Otherwise:
   - Move cursor using tracked `hardware_cursor_row` + `previous_viewport_top`
   - Clear and write only changed lines
   - Handle appended lines vs deletions without scrolling
   - Wrap in synchronized output
5. Update `hardware_cursor_row`, `max_lines_rendered`, `previous_viewport_top`

**Notes:**
- `cursor_row` (logical end of content) is tracked separately from `hardware_cursor_row` and drives viewport calculations (`previous_viewport_top`).
- `clearOnShrink` only when no overlays.
- `isImageLine()` lines skip width checks and reset appends.
- Width overflow checks are only performed in the diff render path (changed lines), not during full render.
- Crash hard if a non‑image line exceeds terminal width (after logging).

---

## ANSI/Unicode width & slicing (exact behavior)

### Crates
- `unicode-segmentation` (grapheme clusters)
- `unicode-width` (East Asian width)
- `unicode-emoji` or `emoji-data` (RGI emoji detection)

### Rules (pi‑tui compatible)
- Width measured per **grapheme cluster**.
- Zero‑width clusters: default‑ignorable/control/mark/surrogate.
- RGI emoji cluster => width **2**.
- Tabs normalized to fixed width (pi uses 3 spaces).
- ANSI sequences (CSI/OSC/APC) are zero‑width.

### Functions to implement
- `visible_width(s: &str) -> usize`
- `slice_by_column(s, start, len, strict)` (strict drops boundary‑overflow wide chars)
- `slice_with_width()` (returns text + visible width)
- `extract_segments()` (before/after segments with inherited styling)
- `wrap_text_with_ansi()` (preserve styling across line breaks)

**Edge cases:**
- OSC‑8 hyperlinks must be ignored in width and reset with `SEGMENT_RESET`.
- Underline bleed: apply `CSI 24 m` at line end when needed (avoid full reset).
- Always enforce final width ≤ terminal width.

---

## Overlays + IME cursor handling (staged additions)

### Overlays
- Overlay stack with `hidden` flag and `visible(width,height)` predicate.
- Showing an overlay stores `preFocus` and focuses the overlay if it is visible; hiding/removing restores focus to the topmost visible overlay or `preFocus`.
- `OverlayHandle.setHidden()` and `visible()` callbacks can shift focus when visibility changes (e.g., resize).
- Positioning by anchor, margins, offsets, and percentage sizing.
- Pad working area to `max(max_lines_rendered, overlay_bottom)` to keep viewport stable.
- Composite lines using `extract_segments()` + `slice_with_width()`.
- Final width verification after compositing.

### IME cursor handling
- Components emit `CURSOR_MARKER` in render output.
- `extract_cursor_position()` scans visible viewport bottom‑up, removes marker.
- `position_hardware_cursor()` uses row delta + absolute column to place IME.

---

## Cleanup / teardown strategy (RAII + signals)

**Correctness‑critical:**
- Disable bracketed paste: `CSI ?2004 l`.
- Disable Kitty protocol: `CSI <u`.
- `stdin.pause()` before leaving raw mode.
- `drainInput()` to prevent key‑release leakage.
- Restore raw mode and cursor visibility.
- TUI `stop()` moves cursor to end of content, prints newline, then restores.

**Rust plan:**
- `TerminalGuard` (Drop) calls `drain_input()` then `stop()`.
- `panic::set_hook` + `signal-hook` (SIGINT/SIGTERM) to ensure cleanup.

---

## What’s required vs staged

**Required for correctness / determinism**
- Terminal RAII + raw mode/protocol cleanup.
- StdinBuffer with escape‑sequence splitting + paste rewrap.
- Kitty protocol query/enable/disable flow.
- Diff renderer with synchronized output + hard width enforcement.
- SEGMENT_RESET for non‑image lines + isImageLine handling.
- IME cursor marker extraction & positioning.
- Cell size query + response parsing for images.
- Env toggles: `PI_HARDWARE_CURSOR`, `PI_CLEAR_ON_SHRINK`, `PI_TUI_WRITE_LOG`, `PI_TUI_DEBUG`, `PI_DEBUG_REDRAW`.

**Stageable (after MVP)**
- Overlay stack + compositing.
- Advanced widgets.
- Image protocol render helpers beyond width/cell‑size handling.

---

If you want, I can draft concrete Rust trait signatures and a minimal `DiffRenderer` implementation skeleton that follows pi‑tui’s exact update algorithm and cursor tracking.

---

## Known divergences (open-ended concerns)
These are documented parity risks in the current snapshot. Either port the exact pi‑tui behavior or add targeted tests that document differences.

- **Width logic:** Rust uses `unicode-width` + `emojis` while pi‑tui uses East‑Asian width + regex-based default‑ignorable handling. Options: port pi‑tui rules or add targeted width tests that codify the divergence.
- **Markdown parsing:** Rust uses the `markdown` crate while pi‑tui uses `marked`. Options: add compatibility fixtures/tests or pursue deeper token‑level parity.
- **Image max-height:** Rust enforces `max_height_cells` sizing in `src/core/terminal_image.rs::render_image`, while pi‑tui TS currently ignores `maxHeightCells` in `packages/tui/src/terminal-image.ts::renderImage` and `packages/tui/src/components/image.ts`. Impact: callers that set a max height will see smaller images (and fewer reserved rows) in Rust than in TS.
