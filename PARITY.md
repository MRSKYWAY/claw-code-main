# PARITY GAP ANALYSIS

Scope: comparison between the original TypeScript source surface described by this repository and the Rust implementation under `rust/crates/`.

Method: compare feature surfaces, registries, entrypoints, runtime plumbing, and the behavior already landed in the Rust port. This document is a roadmap baseline, not a claim of full TypeScript equivalence.

## Executive summary

The Rust port now has a substantially broader foundation than the original parity snapshot recorded earlier in development. Core runtime policy, hooks, plugins, agents, skills, MCP discovery, CLI command handling, local machine-readable output, and deterministic failure handling have all received focused implementation or hardening work.

The project is still **not feature-parity** with the TypeScript CLI. The highest-value remaining gaps are concentrated in subagent/background orchestration, remote/structured transport richness, command-family breadth, and the long tail of TypeScript service integrations.

### Current strengths

- Anthropic/OAuth and multiple provider clients
- Local session persistence, compaction, resume, and runtime status
- Tool calling with explicit permission policy decisions
- PreToolUse/PostToolUse hook execution with denial semantics and bounded timeouts
- `/hooks` inspection and local hook add/remove management backed by workspace-local settings
- Plugin discovery, installation, enable/disable, uninstall/update, hook execution, and bounded tool execution
- Agent lifecycle/concurrency coordination and live agent dispatch
- Local skill and agent discovery across project/user roots
- MCP stdio/bootstrap support, result normalization, and discovered-tool registry integration
- Shared slash-command registry with `/hooks`, `/agents`, `/skills`, and plugin management
- JSON-oriented CLI output with terminal UI suppressed in machine-readable mode
- Local web runtime-status reporting
- Deterministic provider/transport fault-injection coverage
- Runtime-owned subagent registry with parent linkage, lifecycle, cancellation, and result capture

### Remaining major gaps

- External Agent execution is not yet interruptible directly from registry cancellation; registry cancellation is cooperative at the orchestration/state boundary
- Persistent background-task/session history integration
- Broader TypeScript tool families and workflow/system tools
- TypeScript-style remote/structured assistant transport layers
- First-class interactive `/plan`, `/review`, and related command-family parity
- Bundled/MCP-backed skill registry and richer live discovery/reload semantics
- Broader service ecosystem such as analytics, settings sync, policy limits, team memory, notifier, and voice layers
- Full transport- and event-level parity for machine-readable/remote assistant execution

---

## tools/

### Rust status

The Rust tool registry is centralized and now includes built-ins plus plugin- and MCP-discovered tools. Tool execution is integrated with runtime permission policy and PreToolUse/PostToolUse hooks. The Agent tool has explicit lifecycle and bounded concurrency management in the tools crate.

### Remaining gaps

The major TypeScript families still without dedicated Rust equivalents include user-interaction, LSP-driven workflows, several MCP utility commands, remote triggers, scheduling, task/team workflows, and the larger set of workflow/system tools.

**Status:** broad local tool foundation; live Agent lifecycle is synchronized into the runtime subagent registry with nested parent inference and terminal result handoff.

---

## hooks/

### Rust status

Hook configuration is loaded into runtime state, and the live conversation path evaluates policy and executes PreToolUse/PostToolUse hooks. Hook results have explicit Allow/Deny decisions, denial is enforced before tool execution, and hook processes are bounded by configurable timeouts. `/hooks` is now a first-class slash command with local add/remove persistence.

### Remaining gaps

- Broader hook transport/extension model beyond the current command-backed execution path
- Richer remote/team hook management parity

**Status:** runtime and local command management implemented; broader extension/transport parity remains incomplete.

## plugins/

### Rust status

The Rust plugin subsystem now covers discovery and lifecycle management, including install, enable/disable, uninstall, update, bundled-plugin listing, hook execution, and bounded plugin tool execution. Plugin tool input propagation is covered by regression tests.

### Remaining gaps

- No full marketplace/registry UX equivalent to the complete TypeScript ecosystem
- No parity for every TypeScript plugin extension surface
- Plugin-provided command/MCP integration remains narrower than the TypeScript implementation

**Status:** functional plugin subsystem; broader ecosystem parity remains incomplete.

## skills/ and CLAW.md discovery

### Rust status

The Rust CLI exposes `/skills` and direct `claw skills` discovery. Project and user `.codex`/`.claw` skill roots are resolved, legacy `/commands` layouts are recognized, shadowing is reported, and CLAW.md discovery is integrated into prompt construction.

### Remaining gaps

- No full bundled-skill registry equivalent
- No MCP skill-builder pipeline equivalent
- No TypeScript-style live registry/reload/change workflow
- Broader team/session-memory integration around skills is still limited

**Status:** usable local discovery with meaningful parity coverage; registry and dynamic lifecycle parity still missing.

## cli/

### Rust status

The Rust CLI has a shared slash-command registry, local REPL/one-shot prompt flows, session resume, plugin/agent/skill management, model and permission controls, Git/GitHub helpers, and machine-readable JSON output handling. Terminal spinners and streamed tool UI are suppressed in JSON mode.

### Remaining gaps

- First-class interactive `/plan`, `/review`, and other TypeScript command families
- TypeScript-style handler decomposition across the full CLI
- Rich remote/structured transport layers equivalent to `structuredIO`, `remoteIO`, and transport-specific handlers
- Full machine-readable event/stream contract parity across all execution modes
- Persistent subagent task/session history beyond the live registry

**Status:** strong local CLI core; subagent-aware task UX remains a later integration slice.

## assistant/ (agentic loop, streaming, tool calling)

### Rust status

The Rust runtime has a live multi-iteration tool loop, session persistence, permission enforcement, hook-aware tool execution, MCP/plugin tool integration, agent lifecycle coordination, and CLI event rendering. Phase 10A adds a runtime-owned `SubagentRegistry` that tracks parent/child relationships, queued/running/terminal state, cooperative cancellation, and captured results/errors. Phase 10B synchronizes live Agent manifest lifecycle state into that runtime registry. Phase 10C now infers parent edges for nested live Agents, hands terminal output back into registry result capture, and recursively propagates registry cancellation through registered descendants.

### Remaining gaps

- External Agent execution still needs direct cancellation-token feedback so cancellation can interrupt provider work rather than only converging registry state
- No persistent background-task/session-history orchestration comparable to the full TypeScript implementation
- No complete TypeScript-equivalent remote/structured assistant transport stack
- Event-level parity across every structured/remote mode still needs expansion

**Status:** live nested Agent lifecycle and registry semantics are represented consistently; direct interruption and task inspection are the next slices.

## services/ (API client, auth, models, MCP)

### Rust status

Core provider APIs, OAuth, usage accounting, MCP bootstrap/client support, remote upstream proxying, and MCP result normalization are implemented. Discovered MCP tools are wired into the live registry. The MCP inspector exposes configured server inventory without opening connections. Provider and transport failure behavior now has deterministic fault-injection coverage.

### Remaining gaps

- Broader service ecosystem found in TypeScript: analytics, prompt suggestion, session/team memory, settings sync, policy limits, notifier, voice, and related services
- Richer interactive MCP connection-manager/UI behavior
- Provider/model ergonomics and service abstractions remain thinner than TypeScript

**Status:** core service foundation is solid; interactive MCP UX and broader ecosystem parity remain missing.

## recent parity/hardening work

- **Phase 5:** centralized permission/hook policy outcomes and enforced them in the live tool path.
- **Phase 6A/6B:** normalized MCP tool results and wired discovered MCP tools into the registry.
- **Phase 7A:** added skill-resolution regression coverage.
- **Phase 7B/7D:** bounded plugin hook and tool execution with safe timeout ranges.
- **Phase 7E:** verified plugin lifecycle operations.
- **Phase 8A:** added live runtime status endpoint.
- **Phase 8B:** surfaced runtime status in the local web UI.
- **Phase 8C:** verified plugin JSON input propagation through the existing environment contract.
- **Phase 8D:** suppressed terminal spinners and streamed tool UI in machine-readable CLI output modes.
- **Phase 9A:** added first-class `/hooks` command discovery and inspection routing.
- **Phase 9B:** added local `/hooks add` and `/hooks remove` persistence while preserving merged runtime defaults.
- **Phase 9C:** added deterministic provider/transport fault-injection verification, including retry exhaustion and truncated stream handling.
- **Phase 9D:** wired the runtime-backed MCP inspector into the interactive `/mcp` slash-command registry.
- **Phase 10A:** added a runtime-owned subagent orchestration registry with parent linkage, explicit lifecycle transitions, cooperative cancellation, duplicate-ID protection, and terminal result/error capture.
- **Phase 10B:** synchronized live Agent manifest lifecycle state into the runtime subagent registry without rewriting the existing dispatcher.
- **Phase 10C:** propagated nested Agent parent relationships through the existing worker-thread boundary, captured terminal child results from persisted output, and recursively cascaded registry cancellation through descendants without rewriting the large Agent dispatcher.
- **Phase 10D:** exposed registry-backed `/tasks` inspection with deterministic listing, parent/state/result/error rendering, and session-safe ID lookup.

## recommended next implementation targets

1. **Phase 10E — direct Agent cancellation:** wire registry cancellation back into external Agent execution so provider work can terminate cooperatively.
2. Expand persistent background-task/session history and session-safe lookup so provider work can terminate cooperatively.
3. Expand structured/remote assistant transport semantics only after the subagent/task model is represented consistently across local execution modes.
4. Return to richer MCP lifecycle and the broader TypeScript service/tool ecosystem after the subagent architecture is wired through the live dispatcher.