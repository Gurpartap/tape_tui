# AGENTS.md (crates/coding_agent)

This crate inherits repository root rules from `../../AGENTS.md` (repo root).
This file contains **only coding-agent-specific constraints**.

## Local Scope
- Build a pragmatic coding agent in pure Rust.

## Architecture Preferences
- Events are direct method calls on `App` handlers.
- Keep one runtime side-effect seam (`HostOps`-style boundary).
- Avoid action/effect framework ceremony unless complexity requires it.
- Keep code easy to read in one pass.

## Required test focus
- stale run isolation
- cancel idempotency
- inline/no-alt-screen behavior
- deterministic core behavior
- teardown sanity
