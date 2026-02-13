# Upstream provenance

This crate ports code from OpenAI Codex `codex-rs/apply-patch`.

- Upstream repository: https://github.com/openai/codex
- Upstream path: `codex-rs/apply-patch`
- Upstream commit: `dd80e332c45aefc935d68fe067026ccf40312ccd`

## Imported source files

- `src/lib.rs`
- `src/parser.rs`
- `src/invocation.rs`
- `src/seek_sequence.rs` (minimal helper required by `lib.rs`)
- `src/standalone_executable.rs` (minimal helper used by exported `main`)
- `src/main.rs`
- `apply_patch_tool_instructions.md`

## Local modifications

1. Crate/package adaptation for this workspace:
   - package name changed to `apply_patch_engine`
   - binary entrypoint updated to call `apply_patch_engine::main()`
2. Workspace integration files added:
   - local `Cargo.toml`
   - `NOTICE`
   - `LICENSE` (Apache-2.0 text)
3. Local integration regressions in `tests/engine.rs` now cover:
   - parse/apply success and malformed patch failure,
   - deterministic multi-operation summary ordering,
   - move-overwrite semantics,
   - trailing newline normalization,
   - partial-success persistence when later hunks fail.

## Deferred integration backlog (explicitly locked for this issue-closure cycle)

The following parity/features are intentionally deferred and must not be partially implemented in this cycle:

1. Safety approval orchestration parity with upstream patch approval workflows.
2. Shell/unified command interception that rewrites shell/exec apply_patch invocations into dedicated apply_patch runtime flow.
3. Freeform/custom apply_patch mode (non-JSON function-call path).
4. Broader Codex-native tool surface expansion beyond the current v1 mapping.

## Readiness note for current issue-closure scope

Safe now:
- deterministic parser + verification behavior and explicit failure surfaces are covered by engine and wrapper tests,
- host wrapper execution semantics are regression-tested for ordering, path safety, non-mutating verification failures, and repeated-apply predictability,
- runtime stale-run/cancel invariants are explicitly validated with apply_patch-specific contract tests.

Still deferred:
- approval/sandbox parity orchestration,
- shell/unified interception,
- freeform apply_patch mode,
- additional tool-surface expansions.
