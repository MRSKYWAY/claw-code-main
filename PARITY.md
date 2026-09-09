# PARITY GAP ANALYSIS

Scope: comparison between the original TypeScript source surface described by this repository and the Rust implementation under `rust/crates/`.

Method: compare feature surfaces, registries, entrypoints, runtime plumbing, and the behavior already landed in the Rust port. This document is a roadmap baseline, not a claim of full TypeScript equivalence.

## Executive summary

The Rust port now has a substantially broader foundation than the original parity snapshot recorded earlier in development. Core runtime policy, hooks, plugins, agents, skills, MCP discovery, CLI command handling, and machine-readable output have all received focused implementation or hardening work.

The project is still **not feature-parity** with the TypeScript CLI. The highest-value remaining gaps are concentrated in orchestration breadth, remote/structured transport richness, and the long tail of TypeScript service integrations.

### Current strengths

- Anthropic/OAuth and multiple provider clients
- Local session persistence, compaction, resume, and runtime status
- Tool calling with explicit permission policy decisions
- PreToolUse/PostToolUse hook execution with denial semantics and bounded timeouts
- Plugin discovery, installation, enable/disable, uninstall/update, hook execution, and bounded tool execution
- Agent lifecycle/concurrency coordination and live agent dispatch
- Local skill and agent discovery across project/user roots
- MCP stdio/bootstrap support, result normalization, and discovered-tool registry integration
- Shared slash-command registry with `/agents`, `/skills`, and plugin management
- JSON/NDJSON-oriented CLI output path with terminal UI suppressed in machine-readable modes
- Local web runtime-status reporting

### Remaining major gaps

- Broader TypeScript tool families and workflow/system tools
- TypeScript-style remote/structured assistant transport layers
- Full `/hooks`, `/mcp`, `/plan`, `/review`, `/tasks`, and related command-family parity
- Bundled/MCP-backed skill registry and richer live discovery/reload semantics
- Broader service ecosystem such as analytics, settings sync, policy limits, team memory, notifier, and voice layers
- Full transport- and event-level parity for machine-readable/remote assistant execution

---

## tools/

### Rust status

The Rust tool registry is centralized and now includes built-ins plus plugin- and MCP-discovered tools. Tool execution is integrated with runtime permission policy and PreToolUse/PostToolUse hooks.

### Remaining gaps

The major TypeScript families still without dedicated Rust equivalents include user-interaction, LSP-driven workflows, several MCP utility commands, remote triggers, scheduling, task/team workflows, and the larger set of workflow/system tools.

**Status:** broad local tool foundation; still incomplete versus the TypeScript tool catalog.

---

## hooks/

### Rust status

Hook configuration is loaded into runtime state, and the live conversation path evaluates policy and executes PreToolUse/PostToolUse hooks. Hook results have explicit Allow/Deny decisions, denial is enforced before tool execution, and hook processes are bounded by configurable timeouts.

### Remaining gaps

- No dedicated Rust `/hooks` command family yet
- No full TypeScript-style hook management UX
- No broader hook transport/extension model beyond the current command-backed execution path

**Status:** runtime execution is implemented; command/management parity remains incomplete.

---

## plugins/

### Rust status

The Rust plugin subsystem now covers discovery and lifecycle management, including install, enable/disable, uninstall, update, bundled-plugin listing, hook execution, and bounded plugin tool execution. Plugin tool input propagation is covered by regression tests.

### Remaining gaps

- No full marketplace/registry UX equivalent to the complete TypeScript ecosystem
- No parity for every TypeScript plugin extension surface
- Plugin-provided command/MCP integration remains narrower than the TypeScript implementation

**Status:** functional plugin subsystem; broader ecosystem parity remains incomplete.

---

## skills/ and CLAW.md discovery

### Rust status

The Rust CLI exposes `/skills` and direct `claw skills` discovery. Project and user `.codex`/`.claw` skill roots are resolved, legacy `/commands` layouts are recognized, shadowing is reported, and CLAW.md discovery is integrated into prompt construction.

### Remaining gaps

- No full bundled-skill registry equivalent
- No MCP skill-builder pipeline equivalent
- No TypeScript-style live registry/reload/change workflow
- Broader team/session-memory integration around skills is still limited

**Status:** usable local discovery with meaningful parity coverage; registry and dynamic lifecycle parity still missing.

---

## cli/

### Rust status

The Rust CLI has a shared slash-command registry, local REPL/one-shot prompt flows, session resume, plugin/agent/skill management, model and permission controls, Git/GitHub helpers, and machine-readable output handling. JSON/NDJSON output no longer renders terminal spinners or streamed tool UI before the machine payload.

### Remaining gaps

- Dedicated command families such as `/hooks`, `/mcp`, `/plan`, `/review`, `/tasks`, and other TypeScript commands
- TypeScript-style handler decomposition across the full CLI
- Rich remote/structured transport layers equivalent to `structuredIO`, `remoteIO`, and transport-specific handlers
- Full machine-readable event/stream contract parity across all execution modes

**Status:** strong local CLI core; transport and command breadth remain narrower than TypeScript.

---

## assistant/ (agentic loop, streaming, tool calling)

### Rust status

The Rust runtime has a live multi-iteration tool loop, session persistence, permission enforcement, hook-aware tool execution, MCP/plugin tool integration, agent lifecycle coordination, and CLI event rendering. Machine-readable terminal noise has been explicitly suppressed.

### Remaining gaps

- No complete TypeScript-equivalent remote/structured assistant transport stack
- No richer background-task/session-history orchestration comparable to the full TypeScript implementation
- Event-level parity across every structured/remote mode still needs expansion

**Status:** strong core loop and local orchestration; broader transport/orchestration layers remain incomplete.

---

## services/ (API client, auth, models, MCP)

### Rust status

Core provider APIs, OAuth, usage accounting, MCP bootstrap/client support, remote upstream proxying, and MCP result normalization are implemented. Discovered MCP tools are wired into the live registry.

### Remaining gaps

- Broader service ecosystem found in TypeScript: analytics, prompt suggestion, session/team memory, settings sync, policy limits, notifier, voice, and related services
- Richer MCP connection-manager/UI behavior
- Provider/model ergonomics and service abstractions remain thinner than TypeScript

**Status:** core service foundation is solid; broader ecosystem parity is still missing.

---

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

## recommended next implementation targets

1. Add a first-class `/hooks` command/inspection surface around the now-live hook runtime.
2. Expand structured/remote assistant transport semantics beyond local JSON/NDJSON prompt execution.
3. Add the next missing TypeScript command family only after its underlying runtime capability is represented in Rust.
4. Continue closing the service and tool-family gaps with focused, independently testable slices.
