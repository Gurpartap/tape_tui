# pi-tui (Rust) architecture as implemented

This document describes the CURRENT Rust implementation in this repo (not strict TypeScript parity). It focuses on the invariants and behavior that exist in code today, with concrete file pointers.

## Current architecture

### Layers and responsibilities

- `src/core/*`: Pure-ish core types and helpers.
  - Terminal I/O boundary: `src/core/terminal.rs` (`trait Terminal`, `TerminalGuard<T>`)
  - Single output gate: `src/core/output.rs` (`TerminalCmd`, `OutputGate`, `OutputGate::flush`)
  - Cursor marker + extraction: `src/core/cursor.rs` (`CURSOR_MARKER`, `extract_cursor_marker`)
  - Text width/slicing helpers: `src/core/text/*` (e.g. `visible_width` in `src/core/text/width.rs`)
- `src/platform/*`: OS/process terminal integration.
  - Unix terminal implementation: `src/platform/process_terminal.rs` (`ProcessTerminal`)
  - Input buffering and bracketed paste detection: `src/platform/stdin_buffer.rs` (`StdinBuffer`, `StdinEvent`)
  - Crash/panic/signal cleanup plumbing (Unix): `src/platform/process_terminal.rs` (`install_signal_handlers`, `install_panic_hook`, `HookTerminal`)
- `src/render/*`: Renderer and typed frame model.
  - Typed model: `src/render/frame.rs` (`Span`, `Line`, `Frame`)
  - Diff renderer: `src/render/renderer.rs` (`DiffRenderer::render`)
  - Overlay compositing helpers: `src/render/overlay.rs` (`composite_overlays`, layout helpers)
- `src/runtime/*`: Runtime event loop, render scheduling, focus, overlays, IME cursor.
  - Runtime: `src/runtime/tui.rs` (`TuiRuntime`)
  - Hardware cursor moves: `src/runtime/ime.rs` (`position_hardware_cursor`)
  - Focus state: `src/runtime/focus.rs` (`FocusState`)

### Invariants (as enforced by structure)

- Single write gate to the terminal:
  - `src/core/output.rs` documents and enforces the invariant: only `OutputGate::flush(..)` calls `Terminal::write(..)`.
  - `src/lib.rs` repeats this as a crate-level invariant.
- Components/widgets do not write to the terminal:
  - Components return `Vec<String>` from `Component::render(..)` (`src/core/component.rs`) and optional typed cursor metadata via `Component::cursor_pos()`.
  - The runtime and renderer own all terminal output (`src/runtime/tui.rs` + `src/render/renderer.rs`) and route it through `OutputGate`.
- Deterministic output staging:
  - Runtime collects protocol toggles + frame commands + hardware cursor commands and flushes them through one gate (`src/runtime/tui.rs`: `do_render`, `flush_output`).

## Behavioral notes (with file pointers)

### 1) OutputGate flush: small vs large strategy (streaming thresholds)

File: `src/core/output.rs`

- Threshold constants:
  - `OUTPUT_GATE_STREAM_THRESHOLD_BYTES` (64 KiB)
  - `OUTPUT_GATE_STREAM_CHUNK_BYTES` (16 KiB)
- Flush behavior:
  - `OutputGate::flush(..)` computes a conservative encoded length via `encoded_len(..)` over buffered `TerminalCmd`s.
  - If `total_len > OUTPUT_GATE_STREAM_THRESHOLD_BYTES`, it uses the streaming path: `OutputGate::flush_streaming(..)`.
  - Otherwise it coalesces everything into a single `String` and does a single `Terminal::write(..)`.
- Streaming behavior details (`flush_streaming`):
  - `TerminalCmd::Bytes(String)` is written directly (never copied into the coalescing buffer).
  - `TerminalCmd::BytesStatic(&'static str)` that is `>= OUTPUT_GATE_STREAM_CHUNK_BYTES` is written directly.
  - Other commands are encoded into an internal buffer which is flushed when it reaches `OUTPUT_GATE_STREAM_CHUNK_BYTES`.
- Unit test:
  - `flush_streams_large_payloads_without_coalescing` asserts the streaming path is taken for payloads larger than `OUTPUT_GATE_STREAM_THRESHOLD_BYTES`.

### 2) PanicHookGuard composability (process-global wrapper + lock-free cleanup registry)

File: `src/platform/process_terminal.rs`

Background
- Panic hooks are process-global. This crate may be used by multiple independent runtimes within one process.
- Crash cleanup must be best-effort and must not hang.
- The panic hook path must be lock-free (a panic may occur while a mutex is held).

Design
- Instead of stacking per-guard hooks, the platform layer installs a single process-global wrapper hook plus a global registry of cleanup callbacks.
- The wrapper hook is installed when at least one `PanicHookGuard` exists, and removed when the last guard is dropped.

Identity tracking (used for uninstall safety)
- `PanicHookId { data, vtable }` + `panic_hook_id(..)` compute a stable identity for a hook by comparing the fat pointer components.
- On uninstall, identity is used to restore the previous hook only if the currently installed hook is still our wrapper (so we never clobber hooks installed after ours).

Lock-free registry of cleanups
- Global head pointer:
  - `PANIC_CLEANUP_HEAD: AtomicPtr<PanicCleanupNode>`
- Each `PanicCleanupNode` stores:
  - `cleanup: Arc<dyn Fn() + Send + Sync>`
  - `ran: AtomicBool` (cleanup executes at most once per node/process)
  - `active: AtomicBool` (guard drop deactivates the node)
  - `next: AtomicPtr<PanicCleanupNode>`
- Nodes are intentionally leaked for the program lifetime; guard drop only flips `active=false`.
  This avoids any memory reclamation/ABA complexity on the panic path.

Wrapper install/uninstall bookkeeping (not on panic path)
- Global refcount:
  - `PANIC_HOOK_ACTIVE_GUARDS: AtomicUsize`
  - 0 → 1 triggers wrapper install
  - 1 → 0 triggers wrapper uninstall
- `sync_panic_hook_state()` performs the install/uninstall under `PANIC_HOOK_WRAPPER_STATE: Mutex<PanicHookWrapperState>`.
  - The installed wrapper hook itself never locks.
  - The wrapper hook runs `run_all_panic_cleanups()` (atomic traversal) and then delegates to the captured previous hook.

Unix-only unit tests
- `panic_hook_guard_drop_does_not_clobber_later_hooks` validates we do not overwrite a later-installed hook.
- `panic_hook_guards_restore_base_hook_when_dropped_out_of_order` validates that dropping guards out of LIFO order still restores the base hook chain when the last guard is dropped.

### 3) HookTerminal crash-safety and CrashCleanup usage

Files: `src/platform/process_terminal.rs`, `src/runtime/tui.rs`

- Crash-safe terminal handle:
  - `HookTerminal::new` opens `/dev/tty` with `O_WRONLY | O_NONBLOCK | O_NOCTTY | O_CLOEXEC`.
  - There is no stdout/stderr fallback. If opening `/dev/tty` fails, it sets `fd = -1` (output disabled).
- Best-effort write behavior:
  - `HookTerminal::write_best_effort` never panics and never waits for writability.
  - It writes until completion, but returns early (dropping remaining bytes) on `WouldBlock`/EAGAIN or any other non-EINTR error.
- Unix-only unit test:
  - `hook_terminal_write_best_effort_returns_on_would_block` fills a non-blocking pipe until it would block and verifies `write_best_effort(..)` returns.
- Runtime usage (crash cleanup):
  - `src/runtime/tui.rs`: `CrashCleanup::run_best_effort` constructs a `HookTerminal` and calls `CrashCleanup::run(..)` inside `catch_unwind`.
  - `CrashCleanup::run(..)` emits `ShowCursor`, `BracketedPasteDisable`, `KittyDisable` through an `OutputGate` and flushes them to the provided terminal.
  - `TuiRuntime::install_cleanup_hooks` installs both `install_signal_handlers(..)` and `install_panic_hook(..)` with closures that call `CrashCleanup::run_best_effort`.

### 4) Runtime cursor marker compatibility (strip + fallback semantics)

Files: `src/runtime/tui.rs`, `src/core/cursor.rs`

- Marker constant:
  - `src/core/cursor.rs`: `CURSOR_MARKER`
- Extraction and stripping:
  - `src/runtime/tui.rs`: `TuiRuntime::do_render` calls `crate::core::cursor::extract_cursor_marker(&mut lines, height)` to extract a cursor position from rendered lines.
  - It then strips any remaining `CURSOR_MARKER` occurrences from all lines (ensures no marker leaks to the renderer/terminal output).
- Fallback semantics:
  - `TuiRuntime::do_render` uses the extracted marker position only when typed cursor metadata is `None`:
    - `if cursor_pos.is_none() { cursor_pos = extracted_marker_pos; }`
- Runtime tests:
  - `cursor_marker_is_stripped_from_output_and_used_as_fallback_cursor_pos`
  - `cursor_marker_is_stripped_but_cursor_metadata_wins`

### 5) Overlay cursor: skip overlay image lines

File: `src/runtime/tui.rs`

- Cursor selection rejects overlay image lines:
  - `TuiRuntime::composite_overlay_lines` checks `is_image_line(&overlay.lines[cursor_pos.row])` and skips the overlay cursor when that line is an image line.
- Runtime test:
  - `overlay_cursor_is_ignored_when_overlay_line_is_image`

### 6) Clamp cursor column to terminal width

File: `src/runtime/tui.rs`

- Clamp behavior:
  - `TuiRuntime::do_render` clamps `cursor_pos.col` to `width.saturating_sub(1)` before generating hardware cursor moves.
  - This prevents emitting huge `CSI n G` sequences for out-of-range cursor columns.
- Runtime test:
  - `cursor_col_is_clamped_to_terminal_width`

### 7) DiffRenderer: strict-width snapshot + clamp reset behavior

File: `src/render/renderer.rs`

- Strict width is snapshotted once per render call:
  - `DiffRenderer::render` reads `strict_width_enabled()` a single time into `strict_width` (avoids repeated env reads mid-render).
- Clamp behavior avoids duplicating line resets:
  - `SEGMENT_RESET` is appended for non-image lines in `apply_line_resets(..)` before diff rendering.
  - When clamping an overflowing non-image line, `clamp_non_image_line_to_width(..)` strips a trailing `SEGMENT_RESET` before slicing:
    - `line.strip_suffix(SEGMENT_RESET).unwrap_or(line)`
  - It then `slice_by_column(.., strict=true)` and re-appends `SEGMENT_RESET` once.

### 8) Run semantics: explicit `run_blocking_once`, compatibility alias `run`

Files: `src/runtime/tui.rs`, `examples/*`

- Runtime API:
  - `src/runtime/tui.rs`: `TuiRuntime::run_blocking_once()` is the explicit "wait for an event, then process one iteration" API.
  - `src/runtime/tui.rs`: `TuiRuntime::run()` is an alias for compatibility and delegates to `run_blocking_once()`.
- Examples use the explicit API:
  - `examples/ansi-forensics.rs`, `examples/chat-simple.rs`, `examples/markdown-playground.rs` call `run_blocking_once()`.

### 9) Frame flattening + `Line::into_string()` fast path

Files: `src/render/renderer.rs`, `src/render/frame.rs`

- The diff renderer currently flattens typed frames into `Vec<String>` at the renderer boundary:
  - `src/render/renderer.rs`: `DiffRenderer::render` iterates `for line in frame.into_lines()` and calls `line.into_string()`.
- `Line::into_string()` is intentionally optimized because the common case is a single span:
  - `src/render/frame.rs`: `Line::into_string(self)` moves out the inner `String` when `spans.len() == 1` (no alloc/copy).
  - Multi-span lines preallocate total byte capacity before concatenation.
- Unit test:
  - `src/render/frame.rs`: `line_into_string_moves_out_single_span_without_copy` asserts pointer/capacity preservation for the single-span move-out path.

