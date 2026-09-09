use std::collections::BTreeMap;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

const DEFAULT_API_BASE_URL: &str = "https://api.telegram.org";
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const DEFAULT_LEADS_PATH: &str = ".claw/telegram-leads.json";
const MAX_STORED_INTERACTIONS_PER_LEAD: usize = 50;

fn main() {
    if let Err(error) = run() {
        eprintln!("telegram-mcp failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), ServerError> {
    let telegram = TelegramClient::new(read_bot_token(), read_api_base_url())?;
    let mut lead_store = LeadStore::new(read_leads_path())?;
    let mut reader = BufReader::new(io::stdin().lock());
    let mut writer = io::stdout().lock();

    while let Some(payload) = read_framed_message(&mut reader)? {
        let response = match serde_json::from_slice::<JsonRpcRequest>(&payload) {
            Ok(request) => handle_request(&telegram, &mut lead_store, request),
            Err(error) => JsonRpcResponse::error(
                JsonValue::Null,
                -32700,
                format!("invalid JSON-RPC payload: {error}"),
            ),
        };
        write_framed_message(&mut writer, &response)?;
    }

    Ok(())
}

fn read_bot_token() -> Option<String> {
    env::var("TELEGRAM_BOT_TOKEN").ok()
}

fn read_api_base_url() -> String {
    env::var("TELEGRAM_API_BASE_URL")
        .map(|value| normalize_base_url(&value))
        .unwrap_or_else(|_| DEFAULT_API_BASE_URL.to_string())
}

fn read_leads_path() -> PathBuf {
    env::var("TELEGRAM_LEADS_PATH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_leads_path)
}

fn default_leads_path() -> PathBuf {
    env::current_dir()
        .map(|cwd| cwd.join(DEFAULT_LEADS_PATH))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_LEADS_PATH))
}

fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn handle_request(
    telegram: &TelegramClient,
    lead_store: &mut LeadStore,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let id = request.id.unwrap_or(JsonValue::Null);
    match request.method.as_str() {
        "initialize" => {
            let protocol_version = request
                .params
                .and_then(|params| {
                    params
                        .get("protocolVersion")
                        .and_then(JsonValue::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_string());
            JsonRpcResponse::success(
                id,
                json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "telegram-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
        }
        "tools/list" => JsonRpcResponse::success(id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let params = match parse_tool_call_params(request.params) {
                Ok(params) => params,
                Err(message) => return JsonRpcResponse::success(id, tool_error_result(message)),
            };
            JsonRpcResponse::success(
                id,
                execute_tool_call(telegram, lead_store, &params.name, params.arguments),
            )
        }
        method => JsonRpcResponse::error(id, -32601, format!("method not found: {method}")),
    }
}

fn parse_tool_call_params(params: Option<JsonValue>) -> Result<ToolCallParams, String> {
    let params = params.unwrap_or_else(|| json!({}));
    serde_json::from_value::<ToolCallParams>(params)
        .map_err(|error| format!("invalid tools/call params: {error}"))
}

fn execute_tool_call(
    telegram: &TelegramClient,
    lead_store: &mut LeadStore,
    tool_name: &str,
    arguments: Option<JsonValue>,
) -> ToolCallResult {
    match dispatch_tool_call(telegram, lead_store, tool_name, arguments) {
        Ok((summary, value)) => tool_success_result(summary, value),
        Err(error) => tool_error_result(error.to_string()),
    }
}

fn dispatch_tool_call(
    telegram: &TelegramClient,
    lead_store: &mut LeadStore,
    tool_name: &str,
    arguments: Option<JsonValue>,
) -> Result<(String, JsonValue), ToolError> {
    match tool_name {
        "telegram_get_me" => {
            let result = telegram.call("getMe", None)?;
            Ok((summarize_get_me(&result), result))
        }
        "telegram_get_updates" => {
            let mut args = expect_object(arguments)?;
            translate_timeout_seconds(&mut args)?;
            validate_string_array(args.get("allowed_updates"), "allowed_updates")?;
            let result = telegram.call("getUpdates", Some(JsonValue::Object(args)))?;
            Ok((summarize_updates(&result), result))
        }
        "telegram_send_message" => {
            let mut args = expect_object(arguments)?;
            let chat_id = take_required_value(&mut args, "chat_id")?;
            validate_chat_id(&chat_id)?;
            let text = take_required_string(&mut args, "text")?;
            let mut params = JsonMap::new();
            params.insert("chat_id".to_string(), chat_id);
            params.insert("text".to_string(), JsonValue::String(text));
            params.extend(args);
            let result = telegram.call("sendMessage", Some(JsonValue::Object(params)))?;
            Ok((summarize_send_message(&result), result))
        }
        "telegram_get_chat" => {
            let mut args = expect_object(arguments)?;
            let chat_id = take_required_value(&mut args, "chat_id")?;
            validate_chat_id(&chat_id)?;
            let mut params = JsonMap::new();
            params.insert("chat_id".to_string(), chat_id);
            params.extend(args);
            let result = telegram.call("getChat", Some(JsonValue::Object(params)))?;
            Ok((summarize_get_chat(&result), result))
        }
        "telegram_delete_webhook" => {
            let args = expect_object(arguments)?;
            let result = telegram.call("deleteWebhook", Some(JsonValue::Object(args)))?;
            Ok((
                "Deleted the Telegram webhook. Long polling with telegram_get_updates is now available."
                    .to_string(),
                result,
            ))
        }
        "telegram_lead_upsert" => {
            let args = expect_object(arguments)?;
            lead_store.upsert_from_args(args).map_err(Into::into)
        }
        "telegram_lead_get" => {
            let args = expect_object(arguments)?;
            lead_store.get_from_args(args).map_err(Into::into)
        }
        "telegram_lead_list" => {
            let args = expect_object(arguments)?;
            lead_store.list_from_args(args).map_err(Into::into)
        }
        "telegram_lead_record_interaction" => {
            let args = expect_object(arguments)?;
            lead_store
                .record_interaction_from_args(args)
                .map_err(Into::into)
        }
        other => Err(ToolError::InvalidArguments(format!(
            "unknown tool `{other}`"
        ))),
    }
}

fn expect_object(arguments: Option<JsonValue>) -> Result<JsonMap<String, JsonValue>, ToolError> {
    match arguments {
        None => Ok(JsonMap::new()),
        Some(JsonValue::Object(map)) => Ok(map),
        Some(_) => Err(ToolError::InvalidArguments(
            "tool arguments must be a JSON object".to_string(),
        )),
    }
}

fn translate_timeout_seconds(args: &mut JsonMap<String, JsonValue>) -> Result<(), ToolError> {
    let Some(timeout) = args.remove("timeout_seconds") else {
        return Ok(());
    };
    if !(timeout.is_i64() || timeout.is_u64()) {
        return Err(ToolError::InvalidArguments(
            "timeout_seconds must be an integer".to_string(),
        ));
    }
    args.insert("timeout".to_string(), timeout);
    Ok(())
}

fn take_required_value(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<JsonValue, ToolError> {
    args.remove(key).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` is required for this Telegram tool"))
    })
}

fn take_required_string(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<String, ToolError> {
    let value = take_required_value(args, key)?;
    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` must be a string for this Telegram tool"))
    })
}

fn validate_chat_id(value: &JsonValue) -> Result<(), ToolError> {
    if value.is_string() || value.is_i64() || value.is_u64() {
        Ok(())
    } else {
        Err(ToolError::InvalidArguments(
            "`chat_id` must be a string or integer".to_string(),
        ))
    }
}

fn validate_string_array(value: Option<&JsonValue>, key: &str) -> Result<(), ToolError> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(items) = value.as_array() else {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be an array of strings"
        )));
    };
    if items.iter().all(JsonValue::is_string) {
        Ok(())
    } else {
        Err(ToolError::InvalidArguments(format!(
            "`{key}` must be an array of strings"
        )))
    }
}

fn take_store_required_string(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<String, LeadStoreError> {
    let value = args
        .remove(key)
        .ok_or_else(|| LeadStoreError::InvalidInput(format!("`{key}` is required")))?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| LeadStoreError::InvalidInput(format!("`{key}` must be a string")))
}

fn take_store_optional_string(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, LeadStoreError> {
    let Some(value) = args.remove(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| LeadStoreError::InvalidInput(format!("`{key}` must be a string")))
}

fn take_store_optional_string_list(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<Vec<String>>, LeadStoreError> {
    let Some(value) = args.remove(key) else {
        return Ok(None);
    };
    let Some(values) = value.as_array() else {
        return Err(LeadStoreError::InvalidInput(format!(
            "`{key}` must be an array of strings"
        )));
    };
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let string = value.as_str().ok_or_else(|| {
            LeadStoreError::InvalidInput(format!("`{key}` must be an array of strings"))
        })?;
        result.push(string.to_string());
    }
    Ok(Some(result))
}

fn take_store_optional_value_as_string(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, LeadStoreError> {
    let Some(value) = args.remove(key) else {
        return Ok(None);
    };
    match value {
        JsonValue::String(value) => Ok(Some(value)),
        JsonValue::Number(number) => Ok(Some(number.to_string())),
        _ => Err(LeadStoreError::InvalidInput(format!(
            "`{key}` must be a string or integer"
        ))),
    }
}

fn take_store_optional_u64(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<u64>, LeadStoreError> {
    let Some(value) = args.remove(key) else {
        return Ok(None);
    };
    match value {
        JsonValue::Number(number) => number.as_u64().map(Some).ok_or_else(|| {
            LeadStoreError::InvalidInput(format!("`{key}` must be a positive integer"))
        }),
        _ => Err(LeadStoreError::InvalidInput(format!(
            "`{key}` must be a positive integer"
        ))),
    }
}

fn take_store_optional_limit(
    args: &mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<usize>, LeadStoreError> {
    let Some(value) = take_store_optional_u64(args, key)? else {
        return Ok(None);
    };
    usize::try_from(value)
        .map(Some)
        .map_err(|_| LeadStoreError::InvalidInput(format!("`{key}` is too large")))
}

fn normalize_string_list(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn summarize_get_me(result: &JsonValue) -> String {
    let username = result
        .get("username")
        .and_then(JsonValue::as_str)
        .map(|username| format!("@{username}"));
    let first_name = result.get("first_name").and_then(JsonValue::as_str);
    match (username, first_name) {
        (Some(username), Some(first_name)) => {
            format!("Fetched Telegram bot profile for {first_name} ({username}).")
        }
        (Some(username), None) => format!("Fetched Telegram bot profile for {username}."),
        (None, Some(first_name)) => format!("Fetched Telegram bot profile for {first_name}."),
        (None, None) => "Fetched Telegram bot profile.".to_string(),
    }
}

fn summarize_send_message(result: &JsonValue) -> String {
    let chat_id = result
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .map(value_to_string)
        .unwrap_or_else(|| "unknown".to_string());
    let text = result
        .get("text")
        .and_then(JsonValue::as_str)
        .unwrap_or("<non-text message>");
    format!("Sent a Telegram message to chat {chat_id}: {text}")
}

fn summarize_get_chat(result: &JsonValue) -> String {
    let title = result
        .get("title")
        .and_then(JsonValue::as_str)
        .or_else(|| result.get("username").and_then(JsonValue::as_str))
        .or_else(|| result.get("first_name").and_then(JsonValue::as_str));
    let chat_id = result
        .get("id")
        .map(value_to_string)
        .unwrap_or_else(|| "unknown".to_string());
    match title {
        Some(title) => format!("Fetched Telegram chat {chat_id}: {title}."),
        None => format!("Fetched Telegram chat {chat_id}."),
    }
}

fn summarize_updates(result: &JsonValue) -> String {
    let Some(updates) = result.as_array() else {
        return "Fetched Telegram updates.".to_string();
    };
    if updates.is_empty() {
        return "Fetched 0 Telegram updates.".to_string();
    }

    let mut lines = vec![format!("Fetched {} Telegram update(s).", updates.len())];
    for update in updates.iter().take(10) {
        lines.push(format_update(update));
    }
    if updates.len() > 10 {
        lines.push(format!(
            "Showing the first 10 updates. Use `offset` with the last seen update_id + 1 to continue polling."
        ));
    }
    lines.join("\n")
}

fn format_update(update: &JsonValue) -> String {
    let update_id = update
        .get("update_id")
        .map(value_to_string)
        .unwrap_or_else(|| "?".to_string());

    let Some(message_like) = update
        .get("message")
        .or_else(|| update.get("edited_message"))
        .or_else(|| update.get("channel_post"))
        .or_else(|| update.get("edited_channel_post"))
    else {
        return format!("update {update_id}: {}", compact_json(update));
    };

    let chat_id = message_like
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .map(value_to_string)
        .unwrap_or_else(|| "unknown".to_string());
    let sender = message_like
        .get("from")
        .and_then(|from| from.get("username").and_then(JsonValue::as_str))
        .or_else(|| {
            message_like
                .get("from")
                .and_then(|from| from.get("first_name").and_then(JsonValue::as_str))
        })
        .unwrap_or("unknown sender");
    let text = message_like
        .get("text")
        .or_else(|| message_like.get("caption"))
        .and_then(JsonValue::as_str)
        .unwrap_or("<non-text message>");

    format!("update {update_id} chat {chat_id} from {sender}: {text}")
}

fn value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(boolean) => boolean.to_string(),
        JsonValue::Number(number) => number.to_string(),
        JsonValue::String(string) => string.clone(),
        other => compact_json(other),
    }
}

fn compact_json(value: &JsonValue) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_string())
}

fn tool_success_result(summary: String, value: JsonValue) -> ToolCallResult {
    let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| compact_json(&value));
    let text = format!("{summary}\n\n{pretty}");
    ToolCallResult {
        content: vec![ToolContent::text(text)],
        structured_content: Some(value),
        is_error: false,
    }
}

fn tool_error_result(message: String) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::text(message.clone())],
        structured_content: Some(json!({ "error": message })),
        is_error: true,
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "telegram_get_me".to_string(),
            description: "Fetch the current Telegram bot identity and profile information."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "telegram_get_updates".to_string(),
            description: "Read incoming updates for the bot with optional long polling so Claw can monitor new messages."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "offset": {
                        "type": "integer",
                        "description": "Usually the last seen update_id plus 1."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum updates to fetch."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Long-poll timeout in seconds."
                    },
                    "allowed_updates": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Telegram update kinds to return."
                    }
                },
                "additionalProperties": true
            }),
        },
        ToolDefinition {
            name: "telegram_send_message".to_string(),
            description: "Send a Telegram message to a chat the bot can access.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "anyOf": [
                            { "type": "integer" },
                            { "type": "string" }
                        ],
                        "description": "Target chat id or @channelusername."
                    },
                    "text": {
                        "type": "string",
                        "description": "Message text to send."
                    },
                    "parse_mode": {
                        "type": "string",
                        "description": "Optional Telegram parse mode such as MarkdownV2 or HTML."
                    },
                    "disable_notification": {
                        "type": "boolean",
                        "description": "Send without notification when true."
                    }
                },
                "required": ["chat_id", "text"],
                "additionalProperties": true
            }),
        },
        ToolDefinition {
            name: "telegram_get_chat".to_string(),
            description: "Fetch details about a chat, channel, or user the bot can access."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chat_id": {
                        "anyOf": [
                            { "type": "integer" },
                            { "type": "string" }
                        ],
                        "description": "Target chat id or @channelusername."
                    }
                },
                "required": ["chat_id"],
                "additionalProperties": true
            }),
        },
        ToolDefinition {
            name: "telegram_delete_webhook".to_string(),
            description: "Delete the current Telegram webhook so getUpdates long polling can work."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "drop_pending_updates": {
                        "type": "boolean",
                        "description": "Drop queued updates before switching to polling."
                    }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "telegram_lead_upsert".to_string(),
            description: "Create or update a persistent lead record with company, role, sales status, and Telegram context."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lead_id": {
                        "type": "string",
                        "description": "Stable identifier, for example alice-foundry or company-username."
                    },
                    "name": { "type": "string" },
                    "company": { "type": "string" },
                    "role": { "type": "string" },
                    "what_they_do": { "type": "string" },
                    "company_website": { "type": "string" },
                    "telegram_username": { "type": "string" },
                    "telegram_user_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "chat_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "chat_title": { "type": "string" },
                    "source_group": { "type": "string" },
                    "status": { "type": "string" },
                    "next_action": { "type": "string" },
                    "last_summary": { "type": "string" },
                    "notes": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["lead_id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "telegram_lead_get".to_string(),
            description: "Fetch one saved lead record, including recent interaction history."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lead_id": { "type": "string" }
                },
                "required": ["lead_id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "telegram_lead_list".to_string(),
            description: "List saved leads with optional filters for chat, username, tag, status, or free-text search."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lead_id": { "type": "string" },
                    "chat_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "telegram_username": { "type": "string" },
                    "telegram_user_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "status": { "type": "string" },
                    "tag": { "type": "string" },
                    "search": { "type": "string" },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of leads to return. Defaults to 20."
                    }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "telegram_lead_record_interaction".to_string(),
            description: "Append a concise inbound or outbound interaction to a saved lead so the agent remembers chat context across sessions."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lead_id": { "type": "string" },
                    "direction": {
                        "type": "string",
                        "description": "For example inbound, outbound, or note."
                    },
                    "summary": {
                        "type": "string",
                        "description": "Short context summary that should be kept as memory."
                    },
                    "raw_text": {
                        "type": "string",
                        "description": "Optional verbatim message text when you really need it."
                    },
                    "chat_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "message_id": {
                        "anyOf": [{ "type": "integer" }, { "type": "string" }]
                    },
                    "source_group": { "type": "string" },
                    "timestamp_unix_seconds": { "type": "integer" }
                },
                "required": ["lead_id", "summary"],
                "additionalProperties": false
            }),
        },
    ]
}

fn read_framed_message(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return if content_length.is_some() {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading MCP headers",
                ))
            } else {
                Ok(None)
            };
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                let parsed = value.trim().parse::<usize>().map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid Content-Length header: {error}"),
                    )
                })?;
                content_length = Some(parsed);
            }
        }
    }

    let length = content_length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Content-Length header in MCP message",
        )
    })?;
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn write_framed_message(writer: &mut impl Write, response: &impl Serialize) -> io::Result<()> {
    let payload = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", payload.len())?;
    writer.write_all(&payload)?;
    writer.flush()
}

#[derive(Debug)]
struct TelegramClient {
    client: Client,
    token: Option<String>,
    base_url: String,
}

impl TelegramClient {
    fn new(token: Option<String>, base_url: String) -> Result<Self, TelegramError> {
        let client = Client::builder()
            .build()
            .map_err(|error| TelegramError::Http(error.to_string()))?;
        Ok(Self {
            client,
            token,
            base_url: normalize_base_url(&base_url),
        })
    }

    fn call(&self, method: &str, params: Option<JsonValue>) -> Result<JsonValue, TelegramError> {
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| TelegramError::Config("TELEGRAM_BOT_TOKEN is required".to_string()))?;
        let url = format!("{}/bot{token}/{method}", self.base_url);
        let body = params.unwrap_or_else(|| json!({}));
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .map_err(|error| TelegramError::Http(error.to_string()))?;

        let status = response.status();
        let body_text = response
            .text()
            .map_err(|error| TelegramError::Http(error.to_string()))?;

        match serde_json::from_str::<TelegramEnvelope>(&body_text) {
            Ok(envelope) if status.is_success() && envelope.ok => {
                envelope.result.ok_or_else(|| {
                    TelegramError::Parse(format!(
                        "Telegram response for `{method}` was missing a `result` field"
                    ))
                })
            }
            Ok(envelope) => Err(TelegramError::Api {
                method: method.to_string(),
                status: envelope.error_code.or(Some(status.as_u16())),
                description: envelope
                    .description
                    .unwrap_or_else(|| "unknown Telegram API error".to_string()),
            }),
            Err(error) if status.is_success() => Err(TelegramError::Parse(format!(
                "Telegram response parse error for `{method}`: {error}"
            ))),
            Err(_) => Err(TelegramError::Api {
                method: method.to_string(),
                status: Some(status.as_u16()),
                description: body_text,
            }),
        }
    }
}

#[derive(Debug)]
struct LeadStore {
    path: PathBuf,
    data: LeadDatabase,
}

impl LeadStore {
    fn new(path: PathBuf) -> Result<Self, LeadStoreError> {
        let data = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            serde_json::from_str::<LeadDatabase>(&raw).map_err(|error| {
                LeadStoreError::Parse(format!(
                    "failed to parse lead store at {}: {error}",
                    path.display()
                ))
            })?
        } else {
            LeadDatabase::default()
        };

        Ok(Self { path, data })
    }

    fn upsert_from_args(
        &mut self,
        mut args: JsonMap<String, JsonValue>,
    ) -> Result<(String, JsonValue), LeadStoreError> {
        let lead_id = take_store_required_string(&mut args, "lead_id")?;
        let now = now_unix_seconds();
        let lead = self
            .data
            .leads
            .entry(lead_id.clone())
            .or_insert_with(|| LeadRecord {
                lead_id: lead_id.clone(),
                created_at_unix_seconds: now,
                updated_at_unix_seconds: now,
                ..LeadRecord::default()
            });

        if let Some(value) = take_store_optional_string(&mut args, "name")? {
            lead.name = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "company")? {
            lead.company = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "role")? {
            lead.role = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "what_they_do")? {
            lead.what_they_do = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "company_website")? {
            lead.company_website = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "telegram_username")? {
            lead.telegram_username = Some(value.trim_start_matches('@').to_string());
        }
        if let Some(value) = take_store_optional_value_as_string(&mut args, "telegram_user_id")? {
            lead.telegram_user_id = Some(value);
        }
        if let Some(value) = take_store_optional_value_as_string(&mut args, "chat_id")? {
            lead.chat_id = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "chat_title")? {
            lead.chat_title = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "source_group")? {
            lead.source_group = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "status")? {
            lead.status = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "next_action")? {
            lead.next_action = Some(value);
        }
        if let Some(value) = take_store_optional_string(&mut args, "last_summary")? {
            lead.last_summary = Some(value);
        }
        if let Some(mut notes) = take_store_optional_string_list(&mut args, "notes")? {
            lead.notes.append(&mut notes);
            normalize_string_list(&mut lead.notes);
        }
        if let Some(mut tags) = take_store_optional_string_list(&mut args, "tags")? {
            lead.tags.append(&mut tags);
            normalize_string_list(&mut lead.tags);
        }

        if !args.is_empty() {
            let keys = args.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(LeadStoreError::InvalidInput(format!(
                "unsupported lead fields: {keys}"
            )));
        }

        lead.updated_at_unix_seconds = now;
        let lead = lead.clone();
        self.save()?;
        let total = self.data.leads.len();
        Ok((
            format!(
                "Saved lead `{}`. {} lead(s) tracked in {}.",
                lead.lead_id,
                total,
                self.path.display()
            ),
            json!({
                "lead": lead,
                "total": total,
                "storePath": self.path.display().to_string()
            }),
        ))
    }

    fn get_from_args(
        &self,
        mut args: JsonMap<String, JsonValue>,
    ) -> Result<(String, JsonValue), LeadStoreError> {
        let lead_id = take_store_required_string(&mut args, "lead_id")?;
        if !args.is_empty() {
            let keys = args.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(LeadStoreError::InvalidInput(format!(
                "unsupported lead fields: {keys}"
            )));
        }
        let lead =
            self.data.leads.get(&lead_id).cloned().ok_or_else(|| {
                LeadStoreError::NotFound(format!("lead `{lead_id}` was not found"))
            })?;
        Ok((
            format!("Loaded lead `{}` from {}.", lead_id, self.path.display()),
            json!({
                "lead": lead,
                "storePath": self.path.display().to_string()
            }),
        ))
    }

    fn list_from_args(
        &self,
        mut args: JsonMap<String, JsonValue>,
    ) -> Result<(String, JsonValue), LeadStoreError> {
        let limit = take_store_optional_limit(&mut args, "limit")?.unwrap_or(20);
        let filters = LeadQuery {
            lead_id: take_store_optional_string(&mut args, "lead_id")?,
            chat_id: take_store_optional_value_as_string(&mut args, "chat_id")?,
            telegram_username: take_store_optional_string(&mut args, "telegram_username")?
                .map(|value| value.trim_start_matches('@').to_string()),
            telegram_user_id: take_store_optional_value_as_string(&mut args, "telegram_user_id")?,
            status: take_store_optional_string(&mut args, "status")?,
            tag: take_store_optional_string(&mut args, "tag")?,
            search: take_store_optional_string(&mut args, "search")?,
        };

        if !args.is_empty() {
            let keys = args.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(LeadStoreError::InvalidInput(format!(
                "unsupported lead fields: {keys}"
            )));
        }

        let mut leads = self
            .data
            .leads
            .values()
            .filter(|lead| filters.matches(lead))
            .cloned()
            .collect::<Vec<_>>();
        leads.sort_by(|left, right| {
            right
                .updated_at_unix_seconds
                .cmp(&left.updated_at_unix_seconds)
                .then_with(|| left.lead_id.cmp(&right.lead_id))
        });
        if leads.len() > limit {
            leads.truncate(limit);
        }

        Ok((
            format!("Found {} lead(s) in {}.", leads.len(), self.path.display()),
            json!({
                "leads": leads,
                "total": leads.len(),
                "storePath": self.path.display().to_string()
            }),
        ))
    }

    fn record_interaction_from_args(
        &mut self,
        mut args: JsonMap<String, JsonValue>,
    ) -> Result<(String, JsonValue), LeadStoreError> {
        let lead_id = take_store_required_string(&mut args, "lead_id")?;
        let summary = take_store_required_string(&mut args, "summary")?;
        let direction = take_store_optional_string(&mut args, "direction")?
            .unwrap_or_else(|| "note".to_string());
        let raw_text = take_store_optional_string(&mut args, "raw_text")?;
        let chat_id = take_store_optional_value_as_string(&mut args, "chat_id")?;
        let message_id = take_store_optional_value_as_string(&mut args, "message_id")?;
        let source_group = take_store_optional_string(&mut args, "source_group")?;
        let timestamp_unix_seconds = take_store_optional_u64(&mut args, "timestamp_unix_seconds")?
            .unwrap_or_else(now_unix_seconds);

        if !args.is_empty() {
            let keys = args.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(LeadStoreError::InvalidInput(format!(
                "unsupported lead fields: {keys}"
            )));
        }

        let lead =
            self.data.leads.get_mut(&lead_id).ok_or_else(|| {
                LeadStoreError::NotFound(format!("lead `{lead_id}` was not found"))
            })?;

        if let Some(chat_id) = chat_id.clone() {
            lead.chat_id = Some(chat_id);
        }
        if let Some(source_group) = source_group {
            lead.source_group = Some(source_group);
        }

        let interaction = LeadInteraction {
            timestamp_unix_seconds,
            direction,
            summary: summary.clone(),
            raw_text,
            chat_id,
            message_id,
        };
        lead.interactions.push(interaction.clone());
        if lead.interactions.len() > MAX_STORED_INTERACTIONS_PER_LEAD {
            let excess = lead.interactions.len() - MAX_STORED_INTERACTIONS_PER_LEAD;
            lead.interactions.drain(0..excess);
        }
        lead.last_summary = Some(summary);
        lead.last_contact_unix_seconds = Some(timestamp_unix_seconds);
        lead.updated_at_unix_seconds = now_unix_seconds();
        let lead = lead.clone();

        self.save()?;
        Ok((
            format!(
                "Recorded interaction for lead `{}` in {}.",
                lead_id,
                self.path.display()
            ),
            json!({
                "lead": lead,
                "interaction": interaction,
                "storePath": self.path.display().to_string()
            }),
        ))
    }

    fn save(&self) -> Result<(), LeadStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(&self.data).map_err(|error| {
            LeadStoreError::Parse(format!(
                "failed to serialize lead store {}: {error}",
                self.path.display()
            ))
        })?;
        fs::write(&self.path, serialized)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LeadDatabase {
    #[serde(default)]
    leads: BTreeMap<String, LeadRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LeadRecord {
    #[serde(default)]
    lead_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    company: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    what_they_do: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    company_website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telegram_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telegram_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_summary: Option<String>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    interactions: Vec<LeadInteraction>,
    #[serde(default)]
    created_at_unix_seconds: u64,
    #[serde(default)]
    updated_at_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_contact_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeadInteraction {
    timestamp_unix_seconds: u64,
    direction: String,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

#[derive(Debug)]
struct LeadQuery {
    lead_id: Option<String>,
    chat_id: Option<String>,
    telegram_username: Option<String>,
    telegram_user_id: Option<String>,
    status: Option<String>,
    tag: Option<String>,
    search: Option<String>,
}

impl LeadQuery {
    fn matches(&self, lead: &LeadRecord) -> bool {
        if let Some(lead_id) = &self.lead_id {
            if &lead.lead_id != lead_id {
                return false;
            }
        }
        if let Some(chat_id) = &self.chat_id {
            if lead.chat_id.as_ref() != Some(chat_id) {
                return false;
            }
        }
        if let Some(username) = &self.telegram_username {
            if lead.telegram_username.as_ref() != Some(username) {
                return false;
            }
        }
        if let Some(user_id) = &self.telegram_user_id {
            if lead.telegram_user_id.as_ref() != Some(user_id) {
                return false;
            }
        }
        if let Some(status) = &self.status {
            let candidate = lead
                .status
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if candidate != status.to_ascii_lowercase() {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            if !lead
                .tags
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(tag))
            {
                return false;
            }
        }
        if let Some(search) = &self.search {
            let search = search.to_ascii_lowercase();
            let haystacks = [
                Some(lead.lead_id.as_str()),
                lead.name.as_deref(),
                lead.company.as_deref(),
                lead.role.as_deref(),
                lead.what_they_do.as_deref(),
                lead.telegram_username.as_deref(),
                lead.chat_title.as_deref(),
                lead.source_group.as_deref(),
                lead.last_summary.as_deref(),
                lead.next_action.as_deref(),
            ];
            let matches_field = haystacks
                .iter()
                .flatten()
                .any(|value| value.to_ascii_lowercase().contains(&search));
            let matches_note = lead
                .notes
                .iter()
                .any(|value| value.to_ascii_lowercase().contains(&search));
            let matches_interaction = lead
                .interactions
                .iter()
                .any(|interaction| interaction.summary.to_ascii_lowercase().contains(&search));
            if !(matches_field || matches_note || matches_interaction) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug)]
enum LeadStoreError {
    Io(io::Error),
    Parse(String),
    NotFound(String),
    InvalidInput(String),
}

impl Display for LeadStoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(message) | Self::NotFound(message) | Self::InvalidInput(message) => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for LeadStoreError {}

impl From<io::Error> for LeadStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
enum ServerError {
    Io(io::Error),
    Transport(String),
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Transport(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<io::Error> for ServerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<TelegramError> for ServerError {
    fn from(value: TelegramError) -> Self {
        Self::Transport(value.to_string())
    }
}

impl From<LeadStoreError> for ServerError {
    fn from(value: LeadStoreError) -> Self {
        Self::Transport(value.to_string())
    }
}

#[derive(Debug)]
enum TelegramError {
    Config(String),
    Http(String),
    Api {
        method: String,
        status: Option<u16>,
        description: String,
    },
    Parse(String),
}

impl Display for TelegramError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) | Self::Http(message) | Self::Parse(message) => {
                write!(f, "{message}")
            }
            Self::Api {
                method,
                status,
                description,
            } => match status {
                Some(status) => write!(
                    f,
                    "Telegram API call `{method}` failed with status {status}: {description}"
                ),
                None => write!(f, "Telegram API call `{method}` failed: {description}"),
            },
        }
    }
}

impl std::error::Error for TelegramError {}

#[derive(Debug)]
enum ToolError {
    InvalidArguments(String),
    Telegram(TelegramError),
    LeadStore(LeadStoreError),
}

impl Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArguments(message) => write!(f, "{message}"),
            Self::Telegram(error) => write!(f, "{error}"),
            Self::LeadStore(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<TelegramError> for ToolError {
    fn from(value: TelegramError) -> Self {
        Self::Telegram(value)
    }
}

impl From<LeadStoreError> for ToolError {
    fn from(value: LeadStoreError) -> Self {
        Self::LeadStore(value)
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<JsonValue>,
    method: String,
    #[serde(default)]
    params: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn success(id: JsonValue, result: impl Serialize) -> Self {
        let result = serde_json::to_value(result).unwrap_or_else(|error| {
            json!({
                "content": [{
                    "type": "text",
                    "text": format!("failed to serialize MCP result: {error}")
                }],
                "structuredContent": {
                    "error": error.to_string()
                },
                "isError": true
            })
        });
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: JsonValue, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: JsonValue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallResult {
    content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<JsonValue>,
    is_error: bool,
}

#[derive(Debug, Serialize)]
struct ToolContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

impl ToolContent {
    fn text(text: String) -> Self {
        Self { kind: "text", text }
    }
}

#[derive(Debug, Deserialize)]
struct TelegramEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<JsonValue>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<u16>,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        dispatch_tool_call, normalize_base_url, read_api_base_url, read_leads_path, LeadStore,
        TelegramClient, DEFAULT_API_BASE_URL,
    };

    #[derive(Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: serde_json::Value,
    }

    struct MockTelegramServer {
        base_url: String,
        receiver: mpsc::Receiver<RecordedRequest>,
        handle: thread::JoinHandle<()>,
    }

    impl MockTelegramServer {
        fn start(status_line: &'static str, response_body: serde_json::Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let address = listener.local_addr().expect("local addr");
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

                let mut request_line = String::new();
                reader.read_line(&mut request_line).expect("request line");
                let request_parts: Vec<_> = request_line.split_whitespace().collect();
                let method = request_parts.first().expect("method").to_string();
                let path = request_parts.get(1).expect("path").to_string();

                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("header line");
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("Content-Length") {
                            content_length = value.trim().parse().expect("content length");
                        }
                    }
                }

                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).expect("body");
                let body = if body.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&body).expect("json body")
                };

                sender
                    .send(RecordedRequest { method, path, body })
                    .expect("send request");

                let response_text = serde_json::to_string(&response_body).expect("response json");
                write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_text.len(),
                    response_text
                )
                .expect("write response");
                stream.flush().expect("flush response");
            });

            Self {
                base_url: format!("http://{address}"),
                receiver,
                handle,
            }
        }

        fn finish(self) -> RecordedRequest {
            let request = self
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("receive request");
            self.handle.join().expect("join server thread");
            request
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_lead_path() -> PathBuf {
        static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after epoch")
            .as_nanos();
        let sequence = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("telegram-mcp-leads-{nanos}-{sequence}.json"))
    }

    fn temp_lead_store() -> LeadStore {
        LeadStore::new(temp_lead_path()).expect("lead store")
    }

    fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().cloned().expect("json object")
    }

    #[test]
    fn client_posts_to_expected_bot_api_path_and_body() {
        let server = MockTelegramServer::start(
            "200 OK",
            json!({
                "ok": true,
                "result": {
                    "message_id": 7,
                    "text": "hello from claw"
                }
            }),
        );
        let client = TelegramClient::new(Some("test-token".to_string()), server.base_url.clone())
            .expect("client");

        let response = client
            .call(
                "sendMessage",
                Some(json!({
                    "chat_id": 12345,
                    "text": "hello from claw"
                })),
            )
            .expect("sendMessage response");

        assert_eq!(response["message_id"], json!(7));
        let request = server.finish();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/bottest-token/sendMessage");
        assert_eq!(
            request.body,
            json!({
                "chat_id": 12345,
                "text": "hello from claw"
            })
        );
    }

    #[test]
    fn get_updates_translates_timeout_seconds_and_returns_summary() {
        let server = MockTelegramServer::start(
            "200 OK",
            json!({
                "ok": true,
                "result": [{
                    "update_id": 42,
                    "message": {
                        "chat": { "id": 9001 },
                        "from": { "username": "alice" },
                        "text": "ping"
                    }
                }]
            }),
        );
        let client = TelegramClient::new(Some("test-token".to_string()), server.base_url.clone())
            .expect("client");
        let mut lead_store = temp_lead_store();

        let (summary, value) = dispatch_tool_call(
            &client,
            &mut lead_store,
            "telegram_get_updates",
            Some(json!({
                "offset": 43,
                "timeout_seconds": 30,
                "allowed_updates": ["message"]
            })),
        )
        .expect("tool call");

        assert!(summary.contains("Fetched 1 Telegram update(s)."));
        assert_eq!(value[0]["update_id"], json!(42));

        let request = server.finish();
        assert_eq!(request.path, "/bottest-token/getUpdates");
        assert_eq!(
            request.body,
            json!({
                "offset": 43,
                "timeout": 30,
                "allowed_updates": ["message"]
            })
        );
    }

    #[test]
    fn missing_bot_token_returns_tool_error() {
        let client = TelegramClient::new(None, DEFAULT_API_BASE_URL.to_string()).expect("client");
        let mut lead_store = temp_lead_store();

        let error = dispatch_tool_call(&client, &mut lead_store, "telegram_get_me", None)
            .expect_err("tool error");
        assert!(error.to_string().contains("TELEGRAM_BOT_TOKEN"));
    }

    #[test]
    fn api_base_url_and_lead_path_overrides_are_honored() {
        let _guard = env_lock().lock().expect("env lock");
        let original_base = std::env::var("TELEGRAM_API_BASE_URL").ok();
        let original_path = std::env::var("TELEGRAM_LEADS_PATH").ok();
        let temp_path = temp_lead_path();

        std::env::set_var("TELEGRAM_API_BASE_URL", "http://127.0.0.1:8123/custom/");
        std::env::set_var(
            "TELEGRAM_LEADS_PATH",
            temp_path.to_string_lossy().to_string(),
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8123/custom/"),
            "http://127.0.0.1:8123/custom"
        );
        assert_eq!(read_api_base_url(), "http://127.0.0.1:8123/custom");
        assert_eq!(read_leads_path(), temp_path);

        match original_base {
            Some(value) => std::env::set_var("TELEGRAM_API_BASE_URL", value),
            None => std::env::remove_var("TELEGRAM_API_BASE_URL"),
        }
        match original_path {
            Some(value) => std::env::set_var("TELEGRAM_LEADS_PATH", value),
            None => std::env::remove_var("TELEGRAM_LEADS_PATH"),
        }
    }

    #[test]
    fn lead_store_upsert_persists_and_lists_records() {
        let path = temp_lead_path();
        let mut store = LeadStore::new(path.clone()).expect("lead store");

        let (summary, value) = store
            .upsert_from_args(object(json!({
                "lead_id": "alice-foundry",
                "name": "Alice",
                "company": "Foundry Labs",
                "role": "Founder",
                "what_they_do": "Runs tokenization infrastructure for RWA issuers",
                "telegram_username": "@alice",
                "chat_id": "-100123",
                "source_group": "RWA founders",
                "status": "new",
                "next_action": "Send a short ZKCG intro",
                "notes": ["Interested in faster proof generation"],
                "tags": ["rwa", "warm"]
            })))
            .expect("upsert");

        assert!(summary.contains("Saved lead `alice-foundry`"));
        assert_eq!(value["lead"]["company"], json!("Foundry Labs"));

        let reopened = LeadStore::new(path.clone()).expect("reopen lead store");
        let (_, listed) = reopened
            .list_from_args(object(json!({
                "chat_id": "-100123"
            })))
            .expect("list");

        assert_eq!(listed["total"], json!(1));
        assert_eq!(listed["leads"][0]["telegram_username"], json!("alice"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn lead_store_records_interaction_context() {
        let path = temp_lead_path();
        let mut store = LeadStore::new(path.clone()).expect("lead store");

        store
            .upsert_from_args(object(json!({
                "lead_id": "bob-zk",
                "name": "Bob",
                "company": "ZK Ventures",
                "chat_id": "-100555"
            })))
            .expect("upsert");

        let (summary, value) = store
            .record_interaction_from_args(object(json!({
                "lead_id": "bob-zk",
                "direction": "inbound",
                "summary": "Asked whether ZKCG can reduce prover cost for client onboarding flows.",
                "raw_text": "Can this help us lower proof costs for onboarding?",
                "chat_id": "-100555",
                "message_id": 88,
                "source_group": "ZK founders"
            })))
            .expect("record interaction");

        assert!(summary.contains("Recorded interaction for lead `bob-zk`"));
        assert_eq!(value["interaction"]["message_id"], json!("88"));

        let (_, lead) = store
            .get_from_args(object(json!({
                "lead_id": "bob-zk"
            })))
            .expect("get lead");

        assert_eq!(
            lead["lead"]["last_summary"],
            json!("Asked whether ZKCG can reduce prover cost for client onboarding flows.")
        );
        assert_eq!(
            lead["lead"]["interactions"][0]["direction"],
            json!("inbound")
        );
        let _ = fs::remove_file(path);
    }
}
