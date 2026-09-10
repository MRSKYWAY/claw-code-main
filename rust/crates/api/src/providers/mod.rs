use std::future::Future;
use std::pin::Pin;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse};

pub mod claw_provider;
pub mod gemini;
pub mod openai_compat;

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

pub trait Provider {
    type Stream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse>;

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    ClawApi,
    Gemini,
    Xai,
    OpenAi,
    Nvidia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub provider: ProviderKind,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub max_output_tokens: u32,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    pub alias: &'static str,
    pub model: &'static str,
    pub label: &'static str,
    pub metadata: ProviderMetadata,
    pub capabilities: ModelCapabilities,
}

const GEMINI: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::Gemini,
    auth_env: "GEMINI_API_KEY",
    base_url_env: "GEMINI_BASE_URL",
    default_base_url: gemini::DEFAULT_BASE_URL,
};
const NVIDIA: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::Nvidia,
    auth_env: "NVIDIA_API_KEY",
    base_url_env: "NVIDIA_BASE_URL",
    default_base_url: openai_compat::DEFAULT_NVIDIA_BASE_URL,
};

pub const MODEL_CATALOG: &[ModelCatalogEntry] = &[
    ModelCatalogEntry {
        alias: "gemini-flash",
        model: "gemini-3.8-flash",
        label: "Gemini · Flash",
        metadata: GEMINI,
        capabilities: ModelCapabilities { max_output_tokens: 65_536, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
    ModelCatalogEntry {
        alias: "gemini-pro",
        model: "gemini-3.1-pro-preview",
        label: "Gemini · Pro",
        metadata: GEMINI,
        capabilities: ModelCapabilities { max_output_tokens: 65_536, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
    ModelCatalogEntry {
        alias: "nvidia-fast",
        model: "nvidia/nemotron-3.5-lightning-30b-a3b",
        label: "NVIDIA · Fast",
        metadata: NVIDIA,
        capabilities: ModelCapabilities { max_output_tokens: 16_384, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
    ModelCatalogEntry {
        alias: "nvidia-plan",
        model: "moonshotai/kimi-k3",
        label: "NVIDIA · Planner",
        metadata: NVIDIA,
        capabilities: ModelCapabilities { max_output_tokens: 16_384, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
    ModelCatalogEntry {
        alias: "nvidia-agent",
        model: "nvidia/nemotron-3-ultra-550b-a55b",
        label: "NVIDIA · Agent",
        metadata: NVIDIA,
        capabilities: ModelCapabilities { max_output_tokens: 16_384, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
    ModelCatalogEntry {
        alias: "nvidia-long",
        model: "nvidia/nemotron-3-ultra-550b-a55b",
        label: "NVIDIA · Long context",
        metadata: NVIDIA,
        capabilities: ModelCapabilities { max_output_tokens: 16_384, supports_tools: true, supports_streaming: true, supports_reasoning: true },
    },
];

#[must_use]
pub fn resolve_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    MODEL_CATALOG
        .iter()
        .find(|entry| entry.alias == lower)
        .map(|entry| entry.model)
        .map_or_else(|| trimmed.to_string(), ToOwned::to_owned)
}

#[must_use]
pub fn metadata_for_model(model: &str) -> Option<ProviderMetadata> {
    let canonical = resolve_model_alias(model);
    let lower = canonical.to_ascii_lowercase();
    if let Some(entry) = MODEL_CATALOG.iter().find(|entry| entry.model.eq_ignore_ascii_case(&canonical)) {
        return Some(entry.metadata);
    }
    if lower.starts_with("grok") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        });
    }
    if lower.starts_with("gemini") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Gemini,
            auth_env: "GEMINI_API_KEY",
            base_url_env: "GEMINI_BASE_URL",
            default_base_url: gemini::DEFAULT_BASE_URL,
        });
    }
    if is_nvidia_model(&lower) {
        return Some(ProviderMetadata {
            provider: ProviderKind::Nvidia,
            auth_env: "NVIDIA_API_KEY",
            base_url_env: "NVIDIA_BASE_URL",
            default_base_url: openai_compat::DEFAULT_NVIDIA_BASE_URL,
        });
    }
    None
}

#[must_use]
pub fn capabilities_for_model(model: &str) -> ModelCapabilities {
    let canonical = resolve_model_alias(model);
    MODEL_CATALOG
        .iter()
        .find(|entry| entry.model.eq_ignore_ascii_case(&canonical) || entry.alias.eq_ignore_ascii_case(model.trim()))
        .map(|entry| entry.capabilities)
        .unwrap_or(ModelCapabilities {
            max_output_tokens: 64_000,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        })
}

#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    capabilities_for_model(model).max_output_tokens
}

fn is_nvidia_model(model: &str) -> bool {
    ["deepseek-ai/", "z-ai/", "nvidia/", "stepfun-ai/", "minimaxai/", "moonshotai/"]
        .iter()
        .any(|prefix| model.starts_with(prefix))
}

#[must_use]
pub fn detect_provider_kind(model: &str) -> ProviderKind {
    if let Some(metadata) = metadata_for_model(model) {
        return metadata.provider;
    }
    if claw_provider::has_auth_from_env_or_saved().unwrap_or(false) {
        return ProviderKind::ClawApi;
    }
    if openai_compat::has_api_key("OPENAI_API_KEY") {
        return ProviderKind::OpenAi;
    }
    if openai_compat::has_api_key("XAI_API_KEY") {
        return ProviderKind::Xai;
    }
    ProviderKind::ClawApi
}

#[cfg(test)]
mod tests {
    use super::{capabilities_for_model, detect_provider_kind, max_tokens_for_model, resolve_model_alias, ProviderKind};

    #[test]
    fn resolves_gemini_and_nvidia_aliases() {
        assert_eq!(resolve_model_alias("gemini-flash"), "gemini-3.8-flash");
        assert_eq!(resolve_model_alias("gemini-pro"), "gemini-3.1-pro-preview");
        assert_eq!(resolve_model_alias("nvidia-fast"), "nvidia/nemotron-3.5-lightning-30b-a3b");
        assert_eq!(resolve_model_alias("nvidia-agent"), "nvidia/nemotron-3-ultra-550b-a55b");
        assert_eq!(resolve_model_alias("nvidia-plan"), "moonshotai/kimi-k3");
        assert_eq!(resolve_model_alias("nvidia-long"), "nvidia/nemotron-3-ultra-550b-a55b");
    }

    #[test]
    fn detects_provider_from_model_name_first() {
        assert_eq!(detect_provider_kind("gemini-flash"), ProviderKind::Gemini);
        assert_eq!(detect_provider_kind("nvidia-fast"), ProviderKind::Nvidia);
        assert_eq!(detect_provider_kind("nvidia/nemotron-3-ultra-550b-a55b"), ProviderKind::Nvidia);
        assert_eq!(detect_provider_kind("moonshotai/kimi-k3"), ProviderKind::Nvidia);
    }

    #[test]
    fn uses_model_specific_output_limits() {
        assert_eq!(max_tokens_for_model("gemini-flash"), 65_536);
        assert_eq!(max_tokens_for_model("nvidia-fast"), 16_384);
        assert_eq!(max_tokens_for_model("nvidia-plan"), 16_384);
        assert_eq!(max_tokens_for_model("nvidia-agent"), 16_384);
        assert_eq!(max_tokens_for_model("nvidia-long"), 16_384);
    }

    #[test]
    fn unknown_models_keep_safe_defaults() {
        let caps = capabilities_for_model("some-custom-model");
        assert_eq!(caps.max_output_tokens, 64_000);
    }
}
