# Rust Port vs. TypeScript Reference — Comparative Analysis

Comparison of the Rust `tape_tui` port against the TypeScript reference at `pi-mono/packages/tui`.

## Size at a Glance

| Metric | TypeScript (reference) | Rust (port) |
|--------|----------------------|-------------|
| Source LOC | ~9,500 | ~18,700 |
| Test coverage footprint | ~6,500 LOC (17 test files) | `cargo test -- --list`: 245 tests; `cargo test --features unsafe-terminal-access -- --list`: 246 tests |
| Dependencies | `get-east-asian-width`, `Intl.Segmenter`, Node stdlib | 6 crates (libc, signal-hook, unicode-*, emojis, markdown) |
| Files | 23 source files | 30 source files |

---

## Where the Rust Port Is Better

### 1. Output Gate Discipline ★★★

The single biggest architectural improvement. In the TS version, `tui.ts` has **17 direct `terminal.write()`/`hideCursor()`/`showCursor()` calls** scattered across `doRender()`, `stop()`, `showOverlay()`, `hideOverlay()`, `start()`, and `positionHardwareCursor()`. The `Terminal` interface itself exposes `moveBy()`, `hideCursor()`, `showCursor()`, `clearLine()`, `clearScreen()`, `clearFromCursor()` — each of which does a raw `process.stdout.write()`.

The Rust port enforces the invariant structurally: all frame/diff output is staged as typed `TerminalCmd` values and flushed via `OutputGate::flush()`, which is the only place the runtime routes bytes to `Terminal::write(..)`. Out-of-band controls like setting the window title still flow through the same gate, via `TerminalTitleExt::set_title(..)` or the runtime/command APIs. This makes it extremely hard for widgets/components to bypass the output pipeline by accident. The TS version has no such guarantee.

The only explicit opt-out is feature-gated `unsafe-terminal-access`, and the guard is now write-only (`write_raw(..)`) instead of exposing the full `Terminal` lifecycle surface.

### 2. Crash Safety ★★★

The TS version has **zero** crash/signal cleanup. If the process panics or receives `SIGINT`, the terminal is left in raw mode with the cursor hidden and bracketed paste enabled. Users have to blindly type `reset`.

The Rust port has a **comprehensive crash-safety system**:
- `TerminalGuard<T>` — RAII drain-and-stop on drop
- `SignalHookGuard` — `SIGINT`/`SIGTERM`/`SIGHUP` handlers that restore terminal state
- `PanicHookGuard` — lock-free cleanup registry with atomic linked list (nodes are leaked to avoid ABA on the panic path)
- `HookTerminal` — opens `/dev/tty` directly with `O_NONBLOCK` for crash-safe writes that never block and never depend on stdout

This is genuinely production-grade infrastructure that the TS version lacks entirely.

### 3. Typed Output Pipeline ★★

The TS version builds output by string-concatenating raw ANSI escape sequences into a buffer variable:
```typescript
buffer += "\x1b[?2026h";
if (clear) buffer += "\x1b[3J\x1b[2J\x1b[H";
buffer += `\x1b[${lineDiff}B`;
```

The Rust port uses typed `TerminalCmd` variants (`MoveUp(n)`, `HideCursor`, `BracketedPasteEnable`, etc.), so the output pipeline is self-documenting and impossible to malform. Encoding happens in one place (`OutputGate::encode_into`).

### 4. Large-Frame Streaming ★★

The Rust `OutputGate` has a streaming path for payloads > 64KB that avoids doubling peak memory by writing in 16KB chunks. `TerminalCmd::Bytes` payloads are written directly without copying. The TS version always coalesces into a single string, which means a frame with images can cause a large allocation spike.

### 5. Structured Input Events ★★

TS components receive raw `data: string` and must call `matchesKey(data, "ctrl+c")` themselves — repeating parsing work. The Rust port pre-parses into a discriminated `InputEvent` enum (`Key { key_id, event_type }`, `Text { text }`, `Paste { text }`, `Resize { cols, rows }`) so components get structured data and never touch raw escape sequences.

### 6. Typed Frame Model ★

The Rust port introduces `Span` → `Line` → `Frame` as typed containers with metadata (`is_image`), plus `cursor_pos()` returning structured `CursorPos` instead of scanning for a magic marker string. The fast path (`Line::into_string()` moves out the inner `String` without copy for single-span lines) shows attention to allocation.

### 7. Composition over Inheritance ★

TS `TUI extends Container` — the runtime *is* a container. This conflates lifecycle management with the component tree. Rust uses composition: `TuiRuntime<T>` owns a `root: ComponentRc`, cleanly separating the event loop from the component graph.

### 8. Coalesced Render Scheduling Without an Event Loop ★

The TS runtime coalesces via `process.nextTick()` (microtask boundary). The Rust runtime now coalesces within `run_blocking_once()` by draining queued work in a bounded, non-blocking window and rendering at most once per call. This preserves batching semantics without depending on a host event loop, and adds an explicit latency bound via the coalescing budget. `run_once()` remains strict for tests and precise iteration control.

### 9. Runtime Diagnostics for Invalid Mutations ★

The Rust runtime now emits structured diagnostics for mutation-path failures (for example, invalid component/surface IDs and custom command failures) in all builds via `set_on_diagnostic(..)` or stderr fallback. This materially reduces “silent failure” behavior under release builds while preserving deterministic command sequencing.

### 10. Runtime-owned Inline Viewport State ★

The Rust runtime now centralizes inline viewport anchoring/clamp bookkeeping in a dedicated runtime helper (`runtime/inline_viewport.rs`) and recomputes it deterministically on resize/content updates. The TypeScript reference keeps this logic distributed in runtime/app paths.

### 11. Atomic Surface Transaction Commands ★

The Rust runtime exposes an explicit transaction command path (`SurfaceTransactionMutation` +
`Command::SurfaceTransaction`) and ergonomic runtime/runtime-handle/custom-command entrypoints for
ordered multi-mutation surface lifecycle updates. This adds a deterministic batch boundary with one
reconciliation/render decision stage and ordered diagnostics for mixed valid/invalid mutation
payloads. The TypeScript reference does not expose an equivalent first-class transaction payload for
surface lifecycle operations.

### 12. Deterministic Two-Pass Surface Budgeting ★★

The Rust runtime now performs visible-surface sizing with an explicit measure → allocate → render
flow. In constrained terminals, lane reservations are allocated deterministically and clamped to
terminal bounds before any surface render call. This guarantees that `set_viewport_size` receives
final budgets (including zero-row allocations for later lane occupants when earlier lanes consume
available space). The TypeScript reference still resolves overlay sizing/compositing in a single
pass without this explicit runtime negotiation stage.

---

## Where the Rust Port Is Worse

### 1. Test Coverage ★★★

This is the most significant gap. The TS reference has **6,500 LOC** of dedicated test files covering:
- `editor.test.ts` (2,628 LOC) — exhaustive cursor/selection/edit/undo tests
- `overlay-options.test.ts` (538 LOC) — all anchor/margin/percent/clamp combinations
- `stdin-buffer.test.ts` (422 LOC) — partial sequence, paste, timeout edge cases
- `autocomplete.test.ts` (375 LOC) — prefix parsing, quoting, completion application
- `keys.test.ts` (343 LOC) — Kitty + legacy + ambiguity rules
- `tui-render.test.ts` (304 LOC) — diff rendering scenarios
- `markdown.test.ts` (934 LOC) — rendering fixtures
- Plus regression tests, widget tests, style leak tests…

The Rust port now has broad unit + golden coverage across runtime/render/input/widgets (including editor behavior, surface compositing/layout cases, markdown fixtures, stdin-buffer split/timeout/paste edge cases, and output-gate streaming/ordering). That materially reduces earlier blind spots.

The remaining gap is breadth and depth relative to TS’s dedicated end-to-end corpus. The most relevant follow-ups are:
- release-profile coverage in CI (`cargo test --release` matrix for key runtime/diagnostic paths),
- stronger multi-thread dispatch stress fixtures,
- continued adversarial/regression fixture expansion for parser/render boundaries.

### 2. 2x Code Size ★★

18,700 LOC vs 9,500 LOC for the same feature set. Some expansion is inherent to Rust (explicit error handling, type declarations, `impl` blocks), but 2x is on the high end. Contributing factors:
- The crash-safety infrastructure (`process_terminal.rs` is ~1,850 LOC vs the TS's 288 LOC `terminal.ts`)
- The OutputGate/TerminalCmd layer (~500 LOC that doesn't exist in TS)
- Verbose test helpers and test code inline
- The `InputEvent` parsing layer (a new abstraction not in TS)

Some of this is genuinely *better* (OutputGate, crash safety), but it's more surface area to maintain.

### 3. ID-Registry Indirection Complexity ★

The Rust runtime uses a `ComponentId` + registry model for ownership-safe mutation across root/focus/surface paths. It avoids `Rc<RefCell<...>>` borrow panics, but it introduces a different complexity: command payloads can reference stale IDs and must be validated/diagnosed at runtime. This is now handled explicitly, but still adds conceptual overhead versus TS’s direct object references.

## Neutral / Parity Differences

| Aspect | TS | Rust | Notes |
|--------|-----|------|-------|
| Terminal low-level ops | Methods on `Terminal` that write directly to stdout | Helper methods on `TuiRuntime` / `RuntimeHandle` command dispatch that enqueue typed `TerminalCmd`s through the `OutputGate` | Capability parity, but different surface: Rust keeps `Terminal` minimal while preserving deterministic output ordering and renderer bookkeeping. |
| Width calculation | `get-east-asian-width` + `Intl.Segmenter` + LRU cache | `unicode-width` + `unicode-segmentation` + `emojis` crate | Different implementations, both correct. TS uses a 512-entry cache; Rust doesn't (but Rust is faster baseline). |
| Markdown parser | `marked` (not visible in deps, probably workspace) | `markdown` crate | Different parsers, potentially different edge cases. |
| ANSI tracker | `AnsiCodeTracker` class with mutable state | Inline in `text/slice.rs` + `text/ansi.rs` | Same granularity of SGR tracking. |
| Surface compositing | Single-pass overlay layout/compositing in transient-layer path | Two-pass runtime negotiation (measure + allocate) before compositing | Rust adds surface kinds (`Modal`, `Drawer`, `Corner`, `Toast`, `AttachmentRow`) plus deterministic lane-budget allocation and clamped constrained-size behavior. |
| Transient-layer visibility API | `OverlayOptions.visible(termWidth, termHeight)` callback can decide visibility dynamically | `SurfaceVisibility` enum-based checks (`Always`, `MinCols`, `MinSize`) on `SurfaceLayoutOptions` | **Current divergence:** Rust uses deterministic enum-based visibility rules instead of a user callback. |
| Input routing with transient layers | Overlay event handling done through TS runtime layering/focus mechanics | Deterministic capture-first arbitration with internal `Consumed`/`Ignored` bubbling to pre-focus/focused/root fallback targets | Rust now has explicit `SurfaceInputPolicy::{Capture, Passthrough}` semantics for host/extension composition. |
| Surface lifecycle batching | Sequential overlay operations | Explicit ordered transaction payloads (`SurfaceTransactionMutation`) plus single-op APIs | Rust adds first-class atomic batching while preserving single-op paths. |
| Kitty keyboard protocol | Same query → detect → enable flow | Ported faithfully | Same flag 1+2+4 semantics, same base-layout-key fallback logic. |
| Image support | Kitty + iTerm2 detection/encoding | Ported faithfully | Both detect via env vars. |
| Render scheduling boundary | `process.nextTick` microtask boundary | Bounded, non-blocking coalescing window in `run_blocking_once()` | Semantic difference; both coalesce. |

---

## Summary

**The Rust port is architecturally superior** in its output discipline (typed commands through a single gate), crash safety (RAII + lock-free signal/panic cleanup), and input event modeling (structured `InputEvent` enum). These are genuine improvements that make the system more robust and harder to misuse.

**The Rust port still has less test depth than the TypeScript reference**, but it now has substantial deterministic coverage across critical runtime/render/input/widget paths. The highest-value remaining work is release-profile CI validation and additional adversarial end-to-end stress fixtures.

**The 2x code size** is partly justified (crash safety, typed pipeline) and partly inherent to Rust, but it does mean more surface area to audit and maintain.
