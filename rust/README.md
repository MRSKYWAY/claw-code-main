# Claw Code

Claw Code is a local coding-agent CLI implemented in safe Rust. It is **Claude Code inspired** and developed as a **clean-room implementation**: it aims for a strong local agent experience, but it is **not** a direct port or copy of Claude Code.

The Rust workspace is the current main product surface. The `claw` binary provides interactive sessions, one-shot prompts, workspace-aware tools, local agent workflows, and plugin-capable operation from a single workspace.

## Current status

- **Version:** `0.1.0`
- **Release stage:** initial public release, source-build distribution
- **Primary implementation:** Rust workspace in this repository
- **Platform focus:** macOS and Linux developer workstations

## Install, build, and run

### Prerequisites

- Rust stable toolchain
- Cargo
- Provider credentials for the model you want to use

### Authentication

Gemini models:

```bash
export GEMINI_API_KEY="..."
# Optional when targeting a Gemini-compatible base URL or test server
export GEMINI_BASE_URL="https://generativelanguage.googleapis.com/v1beta"
```

NVIDIA Build models:

```bash
export NVIDIA_API_KEY="..."
# Optional when targeting a compatible endpoint or test server
export NVIDIA_BASE_URL="https://integrate.api.nvidia.com/v1"
```

OAuth login is also available:

```bash
cargo run --bin claw -- login
```

### Install locally

```bash
cargo install --path crates/claw-cli --locked
```

### Build from source

```bash
cargo build --release -p claw-cli
```

### Run

From the workspace:

```bash
cargo run --bin claw -- --help
cargo run --bin claw --
cargo run --bin claw -- prompt "summarize this workspace"
cargo run --bin claw -- --model gemini-flash "summarize the current crate layout"
cargo run --bin claw -- --model nvidia-agent "review this Rust crate"
```

From the release build:

```bash
./target/release/claw
./target/release/claw prompt "explain crates/runtime"
```

### Local web UI

Install the local web binary once:

```bash
cargo install --path crates/server --bin claw-web --locked
```

Run the browser interface from the repository you want Claw to inspect:

```bash
claw-web
```

Open `http://127.0.0.1:4317`. The UI runs locally and uses the installed `claw`
executable for each prompt, so it retains the same provider configuration and workspace
permissions as the CLI. Set `CLAW_BIN` to the full executable path if `claw` is not on
your `PATH`; set `CLAW_WEB_PORT` to use a different local port. Sessions and tool activity
are saved after every prompt in `~/.claw/web-sessions.json` (on Windows,
`%USERPROFILE%\\.claw\\web-sessions.json`). Set `CLAW_WEB_STORE` to use a different local
storage file. A web prompt fails clearly after 180 seconds instead of waiting forever; set
`CLAW_WEB_RUN_TIMEOUT_SECS` to a value from `1` to `900` when a slower model needs more time.
The web server embeds the current model catalog at build time, so rebuild or reinstall
`claw-web` after model-catalog changes instead of continuing to run an older binary.

To run it directly from this workspace without installing the binary:

```bash
cargo run -p server --bin claw-web
```

### Multi-agent delegation

Claw can launch asynchronous specialist agents through its `Agent` tool. Their manifests and handoff results are persisted in `.claw-agents`, and the local web UI shows their current state.

- `Explorer` uses `nvidia-fast` for read-only repository discovery.
- `Architect` uses `nvidia-plan` for implementation plans and design tradeoffs.
- `Coder`, `Frontend`, `Backend`, `Tester`, and `Debugger` use `nvidia-agent` for execution.
- `Reviewer` uses `gemini-flash` when configured, otherwise `nvidia-fast`.
- With only Gemini credentials, discovery and review use `gemini-flash`; all other roles use `gemini-pro`.
- Background agents are limited to two concurrent jobs by default. Set `CLAW_MAX_PARALLEL_AGENTS` (1-8) to tune this limit.

Parallelize only read-only discovery and review work. Keep writing agents sequential until a future worktree-based isolation layer is enabled, and give each agent a narrow task plus relevant paths rather than the entire repository.

## Supported capabilities

- Interactive REPL and one-shot prompt execution
- Saved-session inspection and resume flows
- Built-in workspace tools for shell, file read/write/edit, search, web fetch/search, todos, and notebook updates
- Slash commands for status, compaction, config inspection, diff, export, session management, and version reporting
- Local agent and skill discovery with `claw agents` and `claw skills`
- Plugin discovery and management through the CLI and slash-command surfaces
- Gemini and NVIDIA model/provider selection from the command line
- Workspace-aware instruction/config loading (`CLAW.md`, config files, permissions, plugin settings)
- Gemini support uses the standard `generateContent` endpoint with synthesized streaming in v1, so the existing CLI/runtime event loop still works unchanged
- NVIDIA Build models use the OpenAI-compatible chat-completions endpoint. The NVIDIA aliases are `nvidia-fast` (`nvidia/nemotron-3.5-lightning-30b-a3b`), `nvidia-plan` (`moonshotai/kimi-k3`), `nvidia-agent` (`nvidia/nemotron-3-ultra-550b-a55b`), and `nvidia-long` (`nvidia/nemotron-3-ultra-550b-a55b`).
- Gemini aliases are `gemini-flash` (`gemini-3.8-flash`) and `gemini-pro` (`gemini-3.1-pro-preview`). Gemini uses the standard content-generation endpoint with synthesized streaming in this release.
- Run `claw models --check` after setting `NVIDIA_API_KEY` or `GEMINI_API_KEY` to verify current account availability without exposing key material.
- Background agents automatically route to lighter or specialized NVIDIA aliases when `NVIDIA_API_KEY` is configured and no explicit agent model is requested. This keeps exploration and planning work off the local machine and caps each agent type to a smaller iteration budget.
- MCP extensions can be added as standalone stdio servers, including the bundled Telegram Bot API server with persistent lead memory documented in [`docs/telegram-mcp.md`](docs/telegram-mcp.md)

## Current limitations

- Public distribution is **source-build only** today; this workspace is not set up for crates.io publishing
- GitHub CI verifies `cargo check`, `cargo test`, and release builds, but automated release packaging is not yet present
- Current CI targets Ubuntu and macOS; Windows release readiness is still to be established
- Some live-provider integration coverage is opt-in because it requires external credentials and network access
- The command surface may continue to evolve during the `0.x` series

## Implementation

The Rust workspace is the active product implementation. It currently includes these crates:

- `claw-cli` — user-facing binary
- `api` — provider clients and streaming
- `runtime` — sessions, config, permissions, prompts, and runtime loop
- `tools` — built-in tool implementations
- `commands` — slash-command registry and handlers
- `plugins` — plugin discovery, registry, and lifecycle support
- `lsp` — language-server protocol support types and process helpers
- `telegram-mcp` — stdio MCP bridge for Telegram Bot API read/send/poll workflows
- `server` and `compat-harness` — supporting services and compatibility tooling

## Roadmap

- Publish packaged release artifacts for public installs
- Add a repeatable release workflow and longer-lived changelog discipline
- Expand platform verification beyond the current CI matrix
- Add more task-focused examples and operator documentation
- Continue tightening feature coverage and UX polish across the Rust implementation

## Release notes

- Draft 0.1.0 release notes: [`docs/releases/0.1.0.md`](docs/releases/0.1.0.md)

## License

See the repository root for licensing details.
