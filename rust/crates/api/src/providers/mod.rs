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
pub struct ModelCatalogEntry {
    pub alias: &'static str,
    pub model: &'static str,
    pub label: &'static str,
    pub metadata: ProviderMetadata,
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
        model: "gemini-3.7-flash",
        label: "Gemini · Flash",
        metadata: GEMINI,
    },
    ModelCatalogEntry {
        alias: "gemini-pro",
        model: "gemini-3.1-pro-preview",
        label: "Gemini · Pro",
        metadata: GEMINI,
    },
    ModelCatalogEntry {
        alias: "nvidia-fast",
        model: "deepseek-ai/deepseek-v4-flash-0731",
        label: "NVIDIA · Fast",
        metadata: NVIDIA,
    },
    ModelCatalogEntry {
        alias: "nvidia-plan",
        model: "nvidia/nemotron-3-super-120b-a12b",
        label: "NVIDIA · Planner",
        metadata: NVIDIA,
    },
    ModelCatalogEntry {
        alias: "nvidia-agent",
        model: "z-ai/glm-5.2",
        label: "NVIDIA · Agent",
        metadata: NVIDIA,
    },
    ModelCatalogEntry {
        alias: "nvidia-long",
        model: "deepseek-ai/deepseek-v4-pro",
        label: "NVIDIA · Long context",
        metadata: NVIDIA,
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
    if let Some(entry) = MODEL_CATALOG.iter().find(|entry| entry.alias == lower) {
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

fn is_nvidia_model(model: &str) -> bool {
    [
        "deepseek-ai/",
        "z-ai/",
        "nvidia/",
        "stepfun-ai/",
        "minimaxai/",
    ]
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

#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    let canonical = resolve_model_alias(model);
    if canonical.contains("opus") {
        32_000
    } else {
        64_000
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_provider_kind, max_tokens_for_model, resolve_model_alias, ProviderKind};

    #[test]
    fn resolves_gemini_and_nvidia_aliases() {
        assert_eq!(resolve_model_alias("gemini-flash"), "gemini-3.7-flash");
        assert_eq!(resolve_model_alias("gemini-pro"), "gemini-3.1-pro-preview");
        assert_eq!(
            resolve_model_alias("nvidia-fast"),
            "deepseek-ai/deepseek-v4-flash-0731"
        );
        assert_eq!(resolve_model_alias("nvidia-agent"), "z-ai/glm-5.2");
    }

    #[test]
    fn detects_provider_from_model_name_first() {
        assert_eq!(detect_provider_kind("gemini-flash"), ProviderKind::Gemini);
        assert_eq!(detect_provider_kind("nvidia-fast"), ProviderKind::Nvidia);
        assert_eq!(
            detect_provider_kind("deepseek-ai/deepseek-v4-pro"),
            ProviderKind::Nvidia
        );
        assert_eq!(detect_provider_kind("z-ai/glm-5.2"), ProviderKind::Nvidia);
    }

    #[test]
    fn keeps_existing_max_token_heuristic() {
        assert_eq!(max_tokens_for_model("nvidia-agent"), 64_000);
    }
}
