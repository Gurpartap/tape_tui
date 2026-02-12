Use `cm` (CodeMapper) cli tool to read/explore code: When you need to understand code and its dependencies, **use cm** instead of iterative Read/Grep operations.

# AGENTS.md (Repo Root)

This file defines default rules for this repository.
Subdirectory `AGENTS.md` files may add **local overrides**; when present, treat root + local as combined policy.

## Project Scope (repo default)
- Rust terminal UI library focused on deterministic runtime/render behavior.

## Core Principles
- Correctness over cleverness
- Deterministic output (single render gate)
- Inline-first (preserve scrollback)
- Layered architecture: core → render → runtime → widgets
- Zero-surprise teardown (RAII cleanup)

## Workflow Rules
- Read any file fully before modifying it.
- Do not write to terminal output directly from widgets/components—renderer only.
- Prefer explicit error handling over silent fallbacks.

## Implementation Constraints
- Preserve current runtime/render/input behavior contracts unless the change explicitly updates them.
- Backward compatibility or legacy migrations are not required.
- Avoid cyclic dependencies between modules.
- No dynamic imports or runtime codegen.

## Testing
- Add tests alongside new functionality.
- For every new parsing/rendering behavior, add a minimal unit test.
- Prefer deterministic, golden-style tests for renderer output.

## Docs
- Update readme/architecture/capability docs and public Rust API docs (crate/module/item rustdoc) when behavior contracts or public surface area change.

## Commit Message Policy
- Do **not** rely on uncommitted planning/spec files for context.
- Commit messages must be **very detailed** and capture:
  - the architecture summary
  - scope and non-goals
  - explain intent and constraints; do not translate the whole diff into English.
  - the implementation plan or roadmap
  - key invariants and decisions (especially around memory)
- Do **not** mention or reference files that are not committed.
- Assume the commit message is the canonical historical record for future agents/humans.

### Prohibited/avoid language
- Milestone framing: `Phase X`, `later phase`, `next phase`, or plan files in title/body.
- Ambiguous statements that hide scope.
- Filler sections with no concrete information.
