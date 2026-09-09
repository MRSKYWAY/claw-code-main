# Telegram MCP Server

`telegram-mcp` is a small stdio MCP server that lets `claw` use the Telegram Bot API for bot-safe message workflows.

Current tools:

- `telegram_get_me`
- `telegram_get_updates`
- `telegram_send_message`
- `telegram_get_chat`
- `telegram_delete_webhook`
- `telegram_lead_upsert`
- `telegram_lead_get`
- `telegram_lead_list`
- `telegram_lead_record_interaction`

## What It Can Do

- Read bot updates with long polling
- Send messages to chats the bot can access
- Fetch chat metadata
- Switch from webhook delivery back to `getUpdates` polling
- Persist lead context between sessions in a local JSON store
- Track names, companies, roles, tags, status, notes, and recent interaction summaries

## What It Cannot Do

- Read arbitrary private Telegram account history
- Access chats the bot is not a member of
- Bypass Telegram bot permissions or privacy restrictions

## Build

From the Rust workspace:

```bash
cargo build -p telegram-mcp
```

## Configure `claw`

Add an MCP server entry in either:

- project local: `.claw/settings.local.json`
- user level: `~/.claw/settings.json`

### Unix-like example

```json
{
  "mcpServers": {
    "telegram": {
      "command": "/absolute/path/to/rust/target/debug/telegram-mcp",
      "args": [],
      "env": {
        "TELEGRAM_BOT_TOKEN": "123456789:telegram-bot-token",
        "TELEGRAM_LEADS_PATH": "/absolute/path/to/workspace/.claw/telegram-leads.json"
      }
    }
  }
}
```

### Windows example

```json
{
  "mcpServers": {
    "telegram": {
      "command": "D:\\claw\\claw-code-main\\rust\\target\\debug\\telegram-mcp.exe",
      "args": [],
      "env": {
        "TELEGRAM_BOT_TOKEN": "123456789:telegram-bot-token",
        "TELEGRAM_LEADS_PATH": "D:\\claw\\claw-code-main\\rust\\.claw\\telegram-leads.json"
      }
    }
  }
}
```

Optional environment variables:

- `TELEGRAM_API_BASE_URL`
  Use this only when targeting a proxy or local test server. The default is `https://api.telegram.org`.
- `TELEGRAM_LEADS_PATH`
  Optional explicit path for persistent lead memory. The default is `.claw/telegram-leads.json` under the current working directory.

## Usage Notes

- Use `telegram_get_me` first to confirm the bot token is valid.
- If `telegram_get_updates` fails because a webhook is configured, call `telegram_delete_webhook` and retry.
- For monitoring loops, pass `timeout_seconds` to `telegram_get_updates` and keep advancing `offset` with the last seen `update_id + 1`.
- After spotting a relevant founder or prospect, save them with `telegram_lead_upsert`.
- After each meaningful chat, call `telegram_lead_record_interaction` with a short summary instead of dumping whole conversations into memory.
- Use `telegram_lead_list` with `chat_id`, `telegram_username`, `status`, `tag`, or `search` when you want to recover context before replying.

## Sales Copilot Workflow

Recommended operating loop:

1. Poll the group with `telegram_get_updates`.
2. For promising people, create or update a lead with `telegram_lead_upsert`.
3. Save concise inbound and outbound summaries with `telegram_lead_record_interaction`.
4. Before replying again, fetch memory with `telegram_lead_list` or `telegram_lead_get`.
5. Draft first and only auto-send once you trust the workflow.

Suggested fields for each lead:

- `name`
- `company`
- `role`
- `what_they_do`
- `source_group`
- `status`
- `next_action`
- `notes`
- `tags`
- `last_summary`

## Example prompts

```text
Check whether the Telegram bot is connected.
```

```text
Poll Telegram for new messages with telegram_get_updates, wait up to 30 seconds, and summarize anything new.
```

```text
Send "Build finished successfully" to chat_id -1001234567890.
```

```text
Check Telegram for new founder messages, look up any saved lead context, and draft replies for me before sending anything.
```
