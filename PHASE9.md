# Phase 9 — Production Verification

Phase 9 closes the production-verification gap around the Rust runtime rather than adding new runtime features.

## Covered by this gate

- Provider HTTP failures retain status, retryability, and response bodies.
- Retry budgets terminate deterministically on persistent retryable failures.
- Malformed JSON fails explicitly instead of being accepted as a successful response.
- Malformed SSE fails at the stream boundary instead of panicking or being silently ignored.
- A truncated trailing SSE frame after a valid prefix is handled without a process-level failure.
- Connection-level transport failures are classified as retryable HTTP errors.
- Existing provider, CLI, hook, plugin, MCP, and agent-lifecycle regression suites remain part of the workspace test gate.

## Intentionally excluded

Live provider smoke tests remain opt-in because they require credentials and network access. They are not a deterministic CI requirement.

The next parity step after Phase 9 is broader orchestration/transport feature work; this phase establishes deterministic failure behavior around the capabilities already present.
