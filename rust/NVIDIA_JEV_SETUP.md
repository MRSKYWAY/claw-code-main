# NVIDIA + Jev Setup

Claw Code can use NVIDIA Build models and TypeSafe AI Jev as optional integrations. Neither is required for normal Claw Code operation.

## Base setup

From the `rust/` directory:

```bash
cargo check --workspace
cargo run --bin claw -- --help
```

For a normal Claw account, use:

```bash
cargo run --bin claw -- login
```

## NVIDIA Build (optional)

Set `NVIDIA_API_KEY`.

macOS/Linux:

```bash
export NVIDIA_API_KEY="your-key"
```

PowerShell:

```powershell
$env:NVIDIA_API_KEY = "your-key"
```

Useful aliases:

- `nvidia-fast` — lightweight exploration
- `nvidia-plan` — planning and architecture
- `nvidia-agent` — implementation and general execution
- `nvidia-long` — long-context alias

Use `NVIDIA_BASE_URL` only for a compatible custom endpoint.

Check model availability with:

```bash
cargo run --bin claw -- models --check
```

NVIDIA is not a required dependency. When its key is absent, the CLI and background agents prefer another configured provider and otherwise use the normal Claw provider.

## Jev (optional)

Jev is a pre-tool decision gate. It runs only after Claw's existing permission policy has already allowed a tool call.

Set `TYPESAFE_API_KEY` only when you want to enable Jev.

macOS/Linux:

```bash
export TYPESAFE_API_KEY="your-key"
```

PowerShell:

```powershell
$env:TYPESAFE_API_KEY = "your-key"
```

Enable Jev in project-local `.claw/settings.local.json`:

```json
{
  "jev": {
    "enabled": true,
    "model": "jev-1.13.0"
  }
}
```

Use `TYPESAFE_API_BASE_URL` only for a compatible test endpoint.

## Behavior when Jev is absent

No Jev setup is required for normal users.

With no `jev` configuration, or with `"enabled": false`, Claw uses the existing permission and hook pipeline without making Jev requests.

When Jev is explicitly enabled but the key is missing or the service is unavailable, Claw fails closed and blocks the guarded tool call. This prevents an enabled security guard from silently disappearing.

In other words:

- Jev absent/disabled → normal Claw behavior.
- Jev enabled + available → Jev participates in automatic tool decisions.
- Jev enabled + unavailable → guarded tool calls are blocked.

## Recommended setups

### Normal user

```bash
cargo run --bin claw
```

Use `claw login` or configure another supported provider.

### NVIDIA user

```bash
export NVIDIA_API_KEY="your-key"
cargo run --bin claw -- --model nvidia-agent
```

### NVIDIA + Jev user

Set both API keys and enable Jev locally:

```json
{
  "jev": {
    "enabled": true,
    "model": "jev-1.13.0"
  }
}
```

Then run Claw normally.

## Security

Never commit `NVIDIA_API_KEY` or `TYPESAFE_API_KEY`.

Use environment variables for secrets and `.claw/settings.local.json` for machine-local feature configuration.
