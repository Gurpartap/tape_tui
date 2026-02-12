# Runtime Validation Matrix

When changing runtime, rendering, or surface behavior, run this matrix locally before commit.

## Required gate

- `cargo test`
- `cargo check --examples`
- `cargo test --example interactive-shell`
- `cargo test --features unsafe-terminal-access`

## Determinism sentinels

- `cargo test --test runtime_deterministic_soak`
- `for i in $(seq 1 20); do cargo test --test runtime_deterministic_soak deterministic_focus_routing_and_cursor_clamp_repeat_cleanly || break; done`
- `for i in $(seq 1 20); do cargo test --test runtime_deterministic_soak deterministic_visibility_toggle_sequence_remains_stable || break; done`

## API regression guards

- `rg -n "show_overlay\(|OverlayHandle|OverlayId|OverlayOptions" src/lib.rs src/runtime/mod.rs src/runtime/tui.rs README.md`

## Notes

- Flaky or order-dependent output in the deterministic sentinels is a release blocker.
- Surface lifecycle behavior must remain command-ordered and deterministic under same-tick command+input pressure.
- Keep all terminal output through `OutputGate::flush(..)`.
