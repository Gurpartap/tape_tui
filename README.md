# tape_tui - build feature-rich and extensible coding agents in the terminal

[![coverage](https://img.shields.io/badge/coverage-95%25-orange)](tests) [![github repo](https://img.shields.io/badge/github-repo-blue?logo=github)](https://github.com/Gurpartap/tape_tui) [![license](https://img.shields.io/badge/license-MIT-green)](LICENSE)

Deterministic, crash-safe terminal UI kernel with differential rendering.

<img src=assets/screenshot.png />

`tape_tui` is a retained-mode TUI framework designed for transcript-style interfaces (coding agents, chats, REPLs) where you **do not** want fullscreen/alternate-screen ownership semantics. It renders inline (preserving scrollback) and keeps runtime behavior predictable under input, resize, layering, and teardown pressure.

## Run examples

```bash
git clone https://github.com/Gurpartap/tape_tui
cd tape_tui

cargo run --example chat-simple
cargo run --example interactive-shell
cargo run --example markdown-playground
cargo run --example ansi-forensics
```

## Development

Special thanks to [Mario Zechner](https://mariozechner.at) for his work on the excellent [Pi coding agent](https://pi.dev/)!

This crate started as a Rust port with [~100% feature parity](docs/COMPARISON.md) of the [`tui` package](https://github.com/badlogic/pi-mono/blob/main/packages/tui) used by Pi.

`tape_tui` has since evolved with a stronger focus on determinism, surface compositing, typed commands through a single gate, crash-safe teardown (RAII + lock-free signal/panic cleanup), and structured input event modelling.

Point your choice of coding agent at this [README.md](https://github.com/Gurpartap/tape_tui/blob/main/README.md) file to build your own chatty apps with tape_tui. See  [docs/ARCHITECTURE.md](https://github.com/Gurpartap/tape_tui/blob/main/docs/ARCHITECTURE.md) for the internal file structure and implementation details explainer.

The port and further development of Tape TUI was passionately orchestrated by a human (me), and implemented largely by gpt-5.3-codex at xhigh thinking. It comes with extensive test coverage to guarantee all critical invariants during development.

## Highlights

- **Inline-first** transcript rendering (scrollback preserved)
- **Deterministic output** via a single terminal output gate (`OutputGate::flush(..)`)
- ANSI **diff renderer** (fast repaints without clearing the whole screen)
- Inline **images** (renders images in terminals using Kitty or iTerm2 graphics protocols)
- Deterministic inline insert-before fast path (safe eligibility + strict fallback)
- **Surface stack** (drawers/modals/toasts/etc.) with explicit input routing policies
- Deterministic two-pass surface size negotiation (measure → allocate → render)
- Deterministic surface z-order controls (`bring_to_front`, `send_to_back`, `raise`, `lower`)
- **Atomic surface transactions** for ordered multi-mutation lifecycle updates in one runtime command boundary
- Deterministic capture-first bubbling (`Consumed`/`Ignored`) with focused/root fallback
- Runtime-owned inline viewport state (tail anchor + resize clamp)
- Structured input events (Kitty keyboard protocol + legacy fallbacks)
- IME/hardware cursor placement via `cursor_pos()` or `CURSOR_MARKER`
- Crash-safe teardown on Unix (signal + panic cleanup)
- Minimal dependencies; no async runtime; no `crossterm`/`termion`

## Non-goals

- Fullscreen/alternate-screen UIs (use `ratatui` if you want that model)
- A general-purpose layout engine
- A built-in Windows terminal backend (see “Terminal backends”)

## Core concepts

### Runtime (`TUI`)

`TUI<T>` is an alias for `runtime::tui::TuiRuntime<T>`. It owns the terminal backend, input parsing, render scheduling, diff rendering, focus/cursor management, and the surface stack.

The runtime is explicitly driven by your code:

- `start()` / `stop()` manage terminal modes
- `run_blocking_once()` waits for work (input/resize/commands), then renders **at most once**
- `render_now()` is an explicit immediate repaint escape hatch

Inline viewport anchoring/clamp state is runtime-owned (tail-follow by default). Resize events recompute the viewport window deterministically before the next render pass.

### Components (retained mode)

Implement `Component` to create custom UI elements. Components:

- render by returning `Vec<String>` (ANSI text lines) from `render(width)`
- can handle input via `handle_event(&InputEvent)`
- **never write directly to the terminal** (renderer only)

### Surfaces (transient layers)

Surfaces are managed layers shown above the root component (drawers/modals/toasts/corners/etc.). Each surface has:

- a `SurfaceKind` (lane defaults)
- a `SurfaceInputPolicy` (`Capture` or `Passthrough`) for deterministic routing
- a `SurfaceHandle` used to hide/show/close/update options/z-order

Canonical lifecycle on the runtime thread is: register component → `tui.show_surface(...)` → mutate
via `SurfaceHandle` (`set_hidden`, `update_options`, `bring_to_front`, `send_to_back`, `raise`, `lower`).
Background threads can enqueue equivalent mutations through `RuntimeHandle::show_surface(...)`,
`RuntimeHandle::{bring_surface_to_front, send_surface_to_back, raise_surface, lower_surface}`,
or raw `RuntimeHandle::dispatch(...)`.

Runtime input arbitration is deterministic: the topmost visible capture surface is tried first; ignored events then bubble to a deterministic fallback target (previous focus/focused/root).

Surface lifecycle control is available across all runtime mutation paths: direct runtime calls, `SurfaceHandle`, `RuntimeHandle::dispatch(..)` command flow, and custom commands (`CustomCommandCtx` surface mutation helpers). Internally, geometry resolution and compositing are fully surface-native (`render::surface`).

Two-pass sizing contract for visible surfaces:
1. **Measure pass:** derive deterministic per-surface constraints from `SurfaceOptions` (`SurfaceKind` lane + layout width/height constraints).
2. **Allocate pass:** compute deterministic lane reservations and per-surface viewport budgets (width + rows), clamped to terminal bounds.
3. **Render pass:** call `set_viewport_size` with the final allocated budget, render lines, and composite with resolved geometry.

Implications:
- hidden surfaces are excluded from the active budget,
- constrained terminals can yield zero-budget allocations for later lane occupants,
- same surface stack + terminal size => same measured constraints and final allocations.

Z-order mutation contract:
- `bring_to_front` / `send_to_back` perform absolute repositioning,
- `raise` / `lower` perform one-step adjacent swaps,
- edge operations are deterministic no-ops,
- hidden surfaces can be reordered without becoming input owners until visible.

#### Atomic surface transactions

When multiple lifecycle updates must be applied as one deterministic operation, use
`SurfaceTransactionMutation` with `TuiRuntime::surface_transaction(..)` or
`RuntimeHandle::surface_transaction(..)`.

Transaction contract:
- mutations are applied strictly in payload order,
- focus reconciliation happens after the ordered apply stage,
- rendering is requested once for the transaction boundary (unless nothing changed),
- invalid mutation targets produce ordered diagnostics while valid entries continue.

Transactions compose with existing APIs (single-op commands, `SurfaceHandle`, custom commands)
instead of replacing them.

Current non-goals for transaction semantics:
- transaction payloads do not currently include z-order mutation variants (use handle/runtime z-order commands).

Inline insert-before fast path contract (renderer optimization):
- activates only under deterministic safety checks (stable width, no surfaces, no image lines,
  cursor/bookkeeping safety, and pure insertion before the previous viewport),
- emits an optimized scroll-loop + viewport repaint sequence when eligible,
- falls back to the baseline full-redraw path whenever any precondition fails,
- preserves baseline visible semantics while keeping output routed through the same output gate.

### Single output gate (invariant)

All runtime rendering output is staged as typed terminal commands and flushed through `OutputGate::flush()`. This keeps output ordering deterministic and prevents widgets/components from accidentally bypassing the renderer.

For extensions that must write raw escape sequences there is an explicit, feature-gated escape hatch: `unsafe-terminal-access`.

## Terminal backends

`ProcessTerminal` is the provided terminal backend.

- On **Unix** (macOS/Linux), it manages raw mode, bracketed paste, Kitty keyboard protocol, resize signals, and crash-safe cleanup.
- On **non-Unix** targets, `ProcessTerminal` will panic on `start()/stop()/write()`.

If you need a different integration (tests, embedding, Windows), implement the `Terminal` trait and pass your backend to `TUI::new(..)`.

## Getting started

### Render once (static)

```rust
use tape_tui::{ProcessTerminal, Text, TUI};

fn main() -> std::io::Result<()> {
    let mut tui = TUI::new(ProcessTerminal::new());

    let root = tui.register_component(Text::new("hello from tape_tui"));
    tui.set_root(vec![root]);

    tui.start()?;
    tui.render_now();
    tui.stop()
}
```

### Typical interactive loop

```rust
use std::cell::RefCell;
use std::rc::Rc;

use tape_tui::{Component, InputEvent, ProcessTerminal, Text, TUI};

struct App {
    text: Text,
    exit: Rc<RefCell<bool>>,
}

impl Component for App {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.text.render(width)
    }

    fn handle_event(&mut self, event: &InputEvent) {
        if matches!(event, InputEvent::Key { key_id, .. } if key_id == "ctrl+c") {
            *self.exit.borrow_mut() = true;
        }
    }
}

fn main() -> std::io::Result<()> {
    let exit = Rc::new(RefCell::new(false));

    let mut tui = TUI::new(ProcessTerminal::new());
    let app = tui.register_component(App {
        text: Text::new("Press Ctrl+C to exit."),
        exit: exit.clone(),
    });
    tui.set_root(vec![app]);

    tui.start()?;
    while !*exit.borrow() {
        tui.run_blocking_once();
    }
    tui.stop()
}
```

### Showing a surface (drawer)

```rust
use tape_tui::{
    ProcessTerminal, SurfaceInputPolicy, SurfaceKind, SurfaceOptions, Text, TUI,
};

let mut tui = TUI::new(ProcessTerminal::new());
let root = tui.register_component(Text::new("root transcript"));
let drawer = tui.register_component(Text::new("drawer"));
tui.set_root(vec![root]);

let handle = tui.show_surface(
    drawer,
    Some(SurfaceOptions {
        kind: SurfaceKind::Drawer,
        input_policy: SurfaceInputPolicy::Capture,
        ..Default::default()
    }),
);

handle.show();
// ...
handle.close();
```

## Built-in widgets

- `Text`, `TruncatedText`
- `Box`, `Container`, `Spacer`
- `Input`, `Editor` (multiline, autocomplete, undo/redo, keybindings)
- `Markdown`
- `SelectList`, `SettingsList`
- `Image` (Kitty + iTerm2)
- `Loader`, `CancellableLoader`

## Feature flags

- `unsafe-terminal-access`: exposes `TuiRuntime::terminal_guard_unsafe().write_raw(..)`
  - bypasses the output-gate ordering guarantee
  - guard drop requests a full redraw/resync on the next tick

## Build & test

```bash
cargo test
cargo test --features unsafe-terminal-access
```

### Runtime/surface change matrix

When touching runtime, render, or surface behavior, run the dedicated matrix in
`tests/RUNTIME_VALIDATION_MATRIX.md`, including:

```bash
cargo test --test runtime_deterministic_soak
```

### Benchmark markdown syntax highlighting

Use the dedicated benchmark harness to compare markdown render cost with
highlighting enabled (default syntect path) versus disabled (plain no-op
highlighter override):

```bash
cargo run --release --example markdown_highlight_bench
```

The benchmark corpus contains fenced code blocks for 10 different languages:
zig, c++, haskell, ocaml, lisp, c, go, rust, mermaid, and dot.

The benchmark reports:
- cold render cost,
- steady-state cost with a fresh Markdown instance per render,
- stream-like cost with a reused Markdown instance using `set_text` incremental
  updates.

## Credits

This project is made possible by [Pi](https://pi.dev/) (MIT licensed). Thank you to its maintainers and contributors.

## License

MIT © 2026 Gurpartap Singh (https://x.com/Gurpartap)
