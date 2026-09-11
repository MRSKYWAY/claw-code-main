# CLAW.md

This file provides guidance to Claw Code when working with code in this repository.

## Detected stack
- Languages: Rust.
- Frameworks: none detected from the supported starter markers.

## Verification
- Run Rust verification from `rust/`: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- `src/` and `tests/` are both present; update both surfaces together when behavior changes.

## Repository shape
- `rust/` contains the Rust workspace and active CLI/runtime implementation.
- `src/` contains source files that should stay consistent with generated guidance and tests.
- `tests/` contains validation surfaces that should be reviewed alongside code changes.

## Working agreement
- Prefer small, reviewable changes and keep generated bootstrap files aligned with actual repo workflows.
- Keep shared defaults in `.claw.json`; reserve `.claw/settings.local.json` for machine-local overrides.
- Do not overwrite existing `CLAW.md` content automatically; update it intentionally when repo workflows change.

## Path and context boundaries
- Treat the repository workspace root as the default base for relative paths, but never reinterpret an explicit absolute path supplied by the user.
- When a task names an absolute path outside the repository (for example, a source file in another project), use that exact path for the requested read or comparison.
- Keep the current repository root and any external task paths conceptually separate; do not replace an external target with a similarly named file inside this repository.
- In persisted conversations, distinguish the latest user request from older transcript history. Do not repeat or treat duplicated historical user messages as new requests.
- When workspace context and an explicit user path appear to conflict, the explicit user path wins for that file or directory operation.

## Completion and anti-thrashing policy
- Optimize for reaching a completed result and returning the final response rather than maximizing tool usage.
- Stop using tools as soon as the requested change is sufficiently complete and validated.
- After a successful build or test, do not rerun the same validation command unless a subsequent code change could have affected its result.
- Do not repeatedly read the same file, run the same command, or apply the same edit when the previous attempt produced no new information or progress.
- Limit fix/validate cycles for the same issue. After two unsuccessful cycles, stop and report the remaining blocker instead of continuing indefinitely.
- Do not make speculative cleanup or unrelated improvements once the requested task is complete.
- When progress has stalled for several tool calls, stop modifying the workspace and provide the best final response based on the work completed so far.
- Always finish with a direct final response summarizing what changed, what was validated, and any unresolved limitation.
