use crate::error::ApiError;
use crate::providers::claw_provider::{self, AuthSource, ClawApiClient};
use crate::providers::gemini::{self, GeminiClient};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::{self, Provider, ProviderKind};
use crate::types::{MessageRequest, MessageResponse, StreamEvent};

async fn send_via_provider<P: Provider>(
    provider: &P,
    request: &MessageRequest,
) -> Result<MessageResponse, ApiError> {
    provider.send_message(request).await
}

async fn stream_via_provider<P: Provider>(
    provider: &P,
    request: &MessageRequest,
) -> Result<P::Stream, ApiError> {
    provider.stream_message(request).await
}

#[derive(Debug, Clone)]
pub enum ProviderClient {
    ClawApi(ClawApiClient),
    Gemini(GeminiClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
    Nvidia(OpenAiCompatClient),
}

impl ProviderClient {
    pub fn from_model(model: &str) -> Result<Self, ApiError> {
        Self::from_model_with_default_auth(model, None)
    }

    pub fn from_model_with_default_auth(
        model: &str,
        default_auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_model = providers::resolve_model_alias(model);
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::ClawApi => Ok(Self::ClawApi(match default_auth {
                Some(auth) => {
                    ClawApiClient::from_auth(auth).with_base_url(claw_provider::read_base_url())
                }
                None => ClawApiClient::from_env()?,
            })),
            ProviderKind::Gemini => Ok(Self::Gemini(GeminiClient::from_env()?)),
            ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::xai(),
            )?)),
            ProviderKind::OpenAi => Ok(Self::OpenAi(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::openai(),
            )?)),
            ProviderKind::Nvidia => Ok(Self::Nvidia(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::nvidia(),
            )?)),
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::ClawApi(_) => ProviderKind::ClawApi,
            Self::Gemini(_) => ProviderKind::Gemini,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
            Self::Nvidia(_) => ProviderKind::Nvidia,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::ClawApi(client) => send_via_provider(client, request).await,
            Self::Gemini(client) => send_via_provider(client, request).await,
            Self::Xai(client) | Self::OpenAi(client) | Self::Nvidia(client) => {
                send_via_provider(client, request).await
            }
        }
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        match self {
            Self::ClawApi(client) => stream_via_provider(client, request)
                .await
                .map(MessageStream::ClawApi),
            Self::Gemini(client) => stream_via_provider(client, request)
                .await
                .map(MessageStream::Gemini),
            Self::Xai(client) | Self::OpenAi(client) | Self::Nvidia(client) => {
                stream_via_provider(client, request)
                    .await
                    .map(MessageStream::OpenAiCompat)
            }
        }
    }
}

#[derive(Debug)]
pub enum MessageStream {
    ClawApi(claw_provider::MessageStream),
    Gemini(gemini::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::ClawApi(stream) => stream.request_id(),
            Self::Gemini(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::ClawApi(stream) => stream.next_event().await,
            Self::Gemini(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
        }
    }
}

pub use claw_provider::{
    oauth_token_is_expired, resolve_saved_oauth_token, resolve_startup_auth_source, OAuthTokenSet,
};
#[must_use]
pub fn read_base_url() -> String {
    claw_provider::read_base_url()
}

#[must_use]
pub fn read_xai_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::xai())
}

#[must_use]
pub fn read_gemini_base_url() -> String {
    gemini::read_base_url()
}

#[must_use]
pub fn read_nvidia_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::nvidia())
}

#[cfg(test)]
mod tests {
    use crate::providers::{detect_provider_kind, resolve_model_alias, ProviderKind};

    #[test]
    fn resolves_gemini_and_nvidia_aliases() {
        assert_eq!(resolve_model_alias("gemini-flash"), "gemini-3.8-flash");
        assert_eq!(resolve_model_alias("gemini-pro"), "gemini-3.1-pro-preview");
        assert_eq!(resolve_model_alias("nvidia-fast"), "deepseek-ai/deepseek-v4-flash-0731");
    }

    #[test]
    fn provider_detection_prefers_model_family() {
        assert_eq!(
            detect_provider_kind("gemini-3.8-flash"),
            ProviderKind::Gemini
        );
        assert_eq!(detect_provider_kind("nvidia-agent"), ProviderKind::Nvidia);
    }
}
