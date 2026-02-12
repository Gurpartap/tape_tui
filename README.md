# tape_tui

Deterministic, **inline-first** terminal UI kernel for Rust.

`tape_tui` is a retained-mode TUI framework designed for transcript-style interfaces (coding agents, chats, REPLs) where you **do not** want fullscreen/alternate-screen ownership semantics. It renders inline (preserving scrollback) and keeps runtime behavior predictable under input, resize, layering, and teardown pressure.

This crate started as a Rust port of the TypeScript `pi-tui` library and has since evolved with a stronger focus on determinism and crash-safe teardown.

## Highlights

- **Inline-first** transcript rendering (scrollback preserved)
- **Deterministic output** via a single terminal output gate (`OutputGate::flush(..)`)
- ANSI **diff renderer** (fast repaints without clearing the whole screen)
- **Surface stack** (drawers/modals/toasts/etc.) with explicit input routing policies
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
- a `SurfaceHandle` used to hide/show/close/update options

Canonical lifecycle on the runtime thread is: register component → `tui.show_surface(...)` → mutate
via `SurfaceHandle`. Background threads can enqueue equivalent mutations through
`RuntimeHandle::show_surface(...)` / `RuntimeHandle::dispatch(...)`.

Runtime input arbitration is deterministic: the topmost visible capture surface is tried first; ignored events then bubble to a deterministic fallback target (previous focus/focused/root).

Surface lifecycle control is available across all runtime mutation paths: direct runtime calls, `SurfaceHandle`, `RuntimeHandle::dispatch(..)` command flow, and custom commands (`CustomCommandCtx` surface mutation helpers). Internally, geometry resolution and compositing are surface-native (`render::surface`) with no overlay compatibility layer.

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
cargo test --test legacy_surface_guard
cargo test --test runtime_deterministic_soak
```

## Run examples

```bash
cargo run --example chat-simple
cargo run --example interactive-shell
cargo run --example markdown-playground
cargo run --example ansi-forensics
```
