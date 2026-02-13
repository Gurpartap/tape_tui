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
3. Additional local integration tests added in `tests/engine.rs` to cover parse/apply success, malformed patches, context mismatch failure, and add/delete/update path handling.
