mod client;
mod error;
mod health;
mod providers;
mod sse;
mod types;

pub use client::{
    oauth_token_is_expired, read_base_url, read_gemini_base_url, read_nvidia_base_url,
    read_xai_base_url, resolve_saved_oauth_token, resolve_startup_auth_source, MessageStream,
    OAuthTokenSet, ProviderClient,
};
pub use error::ApiError;
pub use health::{classify_error, probe_catalog, probe_model, ModelHealth, ModelHealthStatus};
pub use providers::claw_provider::{AuthSource, ClawApiClient, ClawApiClient as ApiClient};
pub use providers::gemini::GeminiClient;
pub use providers::openai_compat::{OpenAiCompatClient, OpenAiCompatConfig};
pub use providers::{
    capabilities_for_model, detect_provider_kind, max_tokens_for_model, resolve_model_alias,
    ModelCapabilities, ModelCatalogEntry, ProviderKind, MODEL_CATALOG,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};
