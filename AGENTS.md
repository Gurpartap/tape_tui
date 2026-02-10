# AGENTS.md

## Project Scope
- Rust port of pi-tui (terminal UI) with parity against `/Users/gurpartap/Projects/github.com/badlogic/pi-mono/packages/tui`.

## Core Principles
- Correctness over cleverness
- Deterministic output (single render gate)
- Inline-first (preserve scrollback)
- Layered architecture: core → render → runtime → widgets
- Zero-surprise teardown (RAII cleanup)

## Workflow Rules
- Read any file fully before modifying it.
- Keep changes scoped to the current milestone phase.
- Do not write to terminal output directly from widgets/components—renderer only.
- Prefer explicit error handling over silent fallbacks.

## Implementation Constraints
- Maintain parity with pi-tui behavior and edge cases from `/Users/gurpartap/Projects/github.com/badlogic/pi-mono/packages/tui`.
- Backward compatibility or legacy migrations are not required.
- Avoid cyclic dependencies between modules.
- No dynamic imports or runtime codegen.

## Testing
- Add tests alongside new functionality.
- For every new parsing/rendering behavior, add a minimal unit test.
- Prefer deterministic, golden-style tests for renderer output.

## Docs
- Update `/Users/gurpartap/Developer/Incubating/tv/docs{ARCHITECTURE,COMPARISON}.md` when phase boundaries change.

## Commit Message Policy
- Do **not** rely on uncommitted planning/spec files for context.
- Commit messages must be **very detailed** and capture:
  - the architecture summary
  - scope and non-goals
  - the implementation plan or roadmap
  - key invariants and decisions (especially around memory)
- Do **not** mention or reference files that are not committed.
- Assume the commit message is the canonical historical record for future agents/humans.
