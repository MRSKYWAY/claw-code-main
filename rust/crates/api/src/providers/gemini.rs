use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use super::{Provider, ProviderFuture};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const REQUEST_ID_HEADER: &str = "request-id";
const ALT_REQUEST_ID_HEADER: &str = "x-request-id";
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(2);
const DEFAULT_MAX_RETRIES: u32 = 2;
const GEMINI_ENV_VARS: &[&str] = &["GEMINI_API_KEY"];

#[derive(Debug, Clone)]
pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl GeminiClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: read_base_url(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    pub fn from_env() -> Result<Self, ApiError> {
        let Some(api_key) = read_env_non_empty("GEMINI_API_KEY")? else {
            return Err(ApiError::missing_credentials("Gemini", GEMINI_ENV_VARS));
        };
        Ok(Self::new(api_key))
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_retry_policy(
        mut self,
        max_retries: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        self.max_retries = max_retries;
        self.initial_backoff = initial_backoff;
        self.max_backoff = max_backoff;
        self
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };
        let response = self.send_with_retry(&request).await?;
        let request_id = request_id_from_headers(response.headers());
        let payload = response.json::<GeminiGenerateContentResponse>().await?;
        let mut normalized = normalize_response(&request.model, payload)?;
        if request_id.is_some() {
            normalized.request_id = request_id;
        } else if normalized.request_id.is_none() {
            normalized.request_id = request_id;
        }
        Ok(normalized)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let response = self.send_message(request).await?;
        Ok(MessageStream::from_response(response))
    }

    async fn send_with_retry(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let mut attempts = 0;

        let last_error = loop {
            attempts += 1;
            let retryable_error = match self.send_raw_request(request).await {
                Ok(response) => match expect_success(response).await {
                    Ok(response) => return Ok(response),
                    Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                    Err(error) => return Err(error),
                },
                Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                Err(error) => return Err(error),
            };

            if attempts > self.max_retries {
                break retryable_error;
            }

            tokio::time::sleep(self.backoff_for_attempt(attempts)?).await;
        };

        Err(ApiError::RetriesExhausted {
            attempts,
            last_error: Box::new(last_error),
        })
    }

    async fn send_raw_request(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let request_url = generate_content_endpoint(&self.base_url, &request.model);
        self.http
            .post(&request_url)
            .header("content-type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&build_generate_content_request(request))
            .send()
            .await
            .map_err(ApiError::from)
    }

    fn backoff_for_attempt(&self, attempt: u32) -> Result<Duration, ApiError> {
        let Some(multiplier) = 1_u32.checked_shl(attempt.saturating_sub(1)) else {
            return Err(ApiError::BackoffOverflow {
                attempt,
                base_delay: self.initial_backoff,
            });
        };
        Ok(self
            .initial_backoff
            .checked_mul(multiplier)
            .map_or(self.max_backoff, |delay| delay.min(self.max_backoff)))
    }
}

impl Provider for GeminiClient {
    type Stream = MessageStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        Box::pin(async move { self.send_message(request).await })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        Box::pin(async move { self.stream_message(request).await })
    }
}

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    pending: VecDeque<StreamEvent>,
}

impl MessageStream {
    fn from_response(response: MessageResponse) -> Self {
        let request_id = response.request_id.clone();
        let pending = synthesize_stream_events(response);
        Self {
            request_id,
            pending,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        Ok(self.pending.pop_front())
    }
}

#[derive(Debug, Serialize)]
struct GeminiGenerateContentRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
}

impl GeminiPart {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            function_call: None,
            function_response: None,
        }
    }

    fn function_call(name: impl Into<String>, args: Value) -> Self {
        Self {
            text: None,
            function_call: Some(GeminiFunctionCall {
                name: name.into(),
                args: normalize_tool_payload(args),
                id: None,
            }),
            function_response: None,
        }
    }

    fn function_response(name: impl Into<String>, response: Value) -> Self {
        Self {
            text: None,
            function_call: None,
            function_response: Some(GeminiFunctionResponse {
                name: name.into(),
                response: normalize_response_payload(response),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(rename = "parametersJsonSchema")]
    parameters_json_schema: Value,
}

#[derive(Debug, Serialize)]
struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: GeminiFunctionCallingConfig,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionCallingConfig {
    mode: &'static str,
    #[serde(
        rename = "allowedFunctionNames",
        skip_serializing_if = "Option::is_none"
    )]
    allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default = "empty_object")]
    args: Value,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiFunctionResponse {
    name: String,
    response: Value,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    #[serde(rename = "modelVersion")]
    model_version: Option<String>,
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "cachedContentTokenCount", default)]
    cached_content_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorEnvelope {
    error: GeminiErrorBody,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorBody {
    status: Option<String>,
    message: Option<String>,
}

fn build_generate_content_request(request: &MessageRequest) -> GeminiGenerateContentRequest {
    GeminiGenerateContentRequest {
        contents: build_contents(&request.messages),
        system_instruction: request
            .system
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|text| GeminiContent {
                role: None,
                parts: vec![GeminiPart::text(text.clone())],
            }),
        tools: request
            .tools
            .as_ref()
            .filter(|tools| !tools.is_empty())
            .map(|tools| {
                vec![GeminiTool {
                    function_declarations: tools.iter().map(gemini_tool_definition).collect(),
                }]
            }),
        tool_config: request
            .tool_choice
            .as_ref()
            .map(gemini_tool_config)
            .filter(|_| {
                request
                    .tools
                    .as_ref()
                    .is_some_and(|tools| !tools.is_empty())
            }),
        generation_config: Some(GeminiGenerationConfig {
            max_output_tokens: request.max_tokens,
        }),
    }
}

fn build_contents(messages: &[InputMessage]) -> Vec<GeminiContent> {
    let mut tool_names = HashMap::<String, String>::new();
    let mut contents = Vec::new();

    for message in messages {
        let role = if message.role == "assistant" {
            Some("model".to_string())
        } else {
            Some("user".to_string())
        };
        let mut parts = Vec::new();

        for block in &message.content {
            match block {
                InputContentBlock::Text { text } if !text.is_empty() => {
                    parts.push(GeminiPart::text(text.clone()));
                }
                InputContentBlock::Text { .. } => {}
                InputContentBlock::ToolUse { id, name, input } => {
                    tool_names.insert(id.clone(), name.clone());
                    parts.push(GeminiPart::function_call(name.clone(), input.clone()));
                }
                InputContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names
                        .get(tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| tool_use_id.clone());
                    parts.push(GeminiPart::function_response(
                        name,
                        gemini_function_response_payload(content, *is_error),
                    ));
                }
            }
        }

        if !parts.is_empty() {
            contents.push(GeminiContent { role, parts });
        }
    }

    contents
}

fn gemini_tool_definition(tool: &ToolDefinition) -> GeminiFunctionDeclaration {
    GeminiFunctionDeclaration {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters_json_schema: tool.input_schema.clone(),
    }
}

fn gemini_tool_config(tool_choice: &ToolChoice) -> GeminiToolConfig {
    let function_calling_config = match tool_choice {
        ToolChoice::Auto => GeminiFunctionCallingConfig {
            mode: "AUTO",
            allowed_function_names: None,
        },
        ToolChoice::Any => GeminiFunctionCallingConfig {
            mode: "ANY",
            allowed_function_names: None,
        },
        ToolChoice::Tool { name } => GeminiFunctionCallingConfig {
            mode: "ANY",
            allowed_function_names: Some(vec![name.clone()]),
        },
    };

    GeminiToolConfig {
        function_calling_config,
    }
}

fn gemini_function_response_payload(content: &[ToolResultContentBlock], is_error: bool) -> Value {
    let mut response = if let [single] = content {
        match single {
            ToolResultContentBlock::Text { text } => json!({ "content": text }),
            ToolResultContentBlock::Json { value } => normalize_response_payload(value.clone()),
        }
    } else {
        json!({
            "content": content
                .iter()
                .map(|block| match block {
                    ToolResultContentBlock::Text { text } => json!({ "text": text }),
                    ToolResultContentBlock::Json { value } => json!({ "json": value }),
                })
                .collect::<Vec<_>>(),
        })
    };

    if is_error {
        if let Some(object) = response.as_object_mut() {
            object.insert("is_error".to_string(), Value::Bool(true));
        } else {
            response = json!({
                "value": response,
                "is_error": true,
            });
        }
    }

    normalize_response_payload(response)
}

fn normalize_response(
    model: &str,
    response: GeminiGenerateContentResponse,
) -> Result<MessageResponse, ApiError> {
    let candidate = response
        .candidates
        .into_iter()
        .next()
        .ok_or(ApiError::InvalidSseFrame(
            "gemini response missing candidates",
        ))?;
    let content = candidate.content.unwrap_or(GeminiContent {
        role: Some("model".to_string()),
        parts: Vec::new(),
    });
    let mut blocks = Vec::new();

    for (index, part) in content.parts.into_iter().enumerate() {
        if let Some(text) = part.text.filter(|value| !value.is_empty()) {
            blocks.push(OutputContentBlock::Text { text });
        }
        if let Some(function_call) = part.function_call {
            blocks.push(OutputContentBlock::ToolUse {
                id: function_call
                    .id
                    .unwrap_or_else(|| format!("tool_call_{}", index + 1)),
                name: function_call.name,
                input: normalize_tool_payload(function_call.args),
            });
        }
    }

    let inferred_stop_reason = infer_stop_reason(candidate.finish_reason.as_deref(), &blocks);

    Ok(MessageResponse {
        id: response
            .response_id
            .clone()
            .unwrap_or_else(|| "gemini_response".to_string()),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: blocks,
        model: response.model_version.unwrap_or_else(|| model.to_string()),
        stop_reason: Some(inferred_stop_reason),
        stop_sequence: None,
        usage: usage_from_metadata(response.usage_metadata),
        request_id: response.response_id,
    })
}

fn usage_from_metadata(usage_metadata: Option<GeminiUsageMetadata>) -> Usage {
    usage_metadata.map_or(
        Usage {
            input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: 0,
        },
        |usage| Usage {
            input_tokens: usage.prompt_token_count,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: usage.cached_content_token_count,
            output_tokens: usage.candidates_token_count,
        },
    )
}

fn infer_stop_reason(finish_reason: Option<&str>, blocks: &[OutputContentBlock]) -> String {
    match finish_reason {
        Some("MAX_TOKENS") => "max_tokens".to_string(),
        Some("STOP") | Some("FINISH_REASON_UNSPECIFIED") | None => {
            if blocks
                .iter()
                .any(|block| matches!(block, OutputContentBlock::ToolUse { .. }))
            {
                "tool_use".to_string()
            } else {
                "end_turn".to_string()
            }
        }
        Some(other) => other.to_ascii_lowercase(),
    }
}

fn synthesize_stream_events(response: MessageResponse) -> VecDeque<StreamEvent> {
    let mut pending = VecDeque::new();
    let mut message = response.clone();
    message.content.clear();
    message.stop_reason = None;
    message.stop_sequence = None;
    message.usage = Usage {
        input_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        output_tokens: 0,
    };

    pending.push_back(StreamEvent::MessageStart(MessageStartEvent { message }));

    let mut block_index = 0_u32;
    for block in &response.content {
        match block {
            OutputContentBlock::Text { text } if !text.is_empty() => {
                pending.push_back(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                    index: block_index,
                    content_block: OutputContentBlock::Text {
                        text: String::new(),
                    },
                }));
                pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: block_index,
                    delta: ContentBlockDelta::TextDelta { text: text.clone() },
                }));
                pending.push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                    index: block_index,
                }));
                block_index += 1;
            }
            OutputContentBlock::ToolUse { id, name, input } => {
                pending.push_back(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                    index: block_index,
                    content_block: OutputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: json!({}),
                    },
                }));
                pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: block_index,
                    delta: ContentBlockDelta::InputJsonDelta {
                        partial_json: input.to_string(),
                    },
                }));
                pending.push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                    index: block_index,
                }));
                block_index += 1;
            }
            OutputContentBlock::Text { .. }
            | OutputContentBlock::Thinking { .. }
            | OutputContentBlock::RedactedThinking { .. } => {}
        }
    }

    pending.push_back(StreamEvent::MessageDelta(MessageDeltaEvent {
        delta: MessageDelta {
            stop_reason: response.stop_reason.clone(),
            stop_sequence: response.stop_sequence.clone(),
        },
        usage: response.usage.clone(),
    }));
    pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));
    pending
}

fn normalize_tool_payload(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "value": value })
    }
}

fn empty_object() -> Value {
    json!({})
}

fn normalize_response_payload(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "value": value })
    }
}

fn generate_content_endpoint(base_url: &str, model: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(":generateContent") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models/{model}:generateContent")
    }
}

fn request_id_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(ALT_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let parsed_error = serde_json::from_str::<GeminiErrorEnvelope>(&body).ok();
    let retryable = is_retryable_status(status);

    Err(ApiError::Api {
        status,
        error_type: parsed_error
            .as_ref()
            .and_then(|error| error.error.status.clone()),
        message: parsed_error
            .as_ref()
            .and_then(|error| error.error.message.clone()),
        body,
        retryable,
    })
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
            | reqwest::StatusCode::INTERNAL_SERVER_ERROR
            | reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[must_use]
pub fn read_base_url() -> String {
    std::env::var("GEMINI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}
