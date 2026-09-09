use std::time::Instant;

use crate::error::ApiError;
use crate::providers::{self, MODEL_CATALOG};
use crate::types::{InputContentBlock, InputMessage, MessageRequest};
use super::ProviderClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelHealthStatus {
    Ready,
    MissingCredentials,
    InvalidModel,
    RateLimited,
    ProviderUnavailable,
    RequestFailed,
}

impl ModelHealthStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::MissingCredentials => "missing_credentials",
            Self::InvalidModel => "invalid_model",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::RequestFailed => "request_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelHealth {
    pub model: String,
    pub status: ModelHealthStatus,
    pub latency_ms: Option<u128>,
    pub detail: String,
}

#[must_use]
pub fn classify_error(error: &ApiError) -> ModelHealthStatus {
    match error {
        ApiError::MissingCredentials { .. } | ApiError::ExpiredOAuthToken | ApiError::Auth(_) => {
            ModelHealthStatus::MissingCredentials
        }
        ApiError::Api { status, .. } if status.as_u16() == 401 || status.as_u16() == 403 => {
            ModelHealthStatus::MissingCredentials
        }
        ApiError::Api { status, .. } if status.as_u16() == 404 => ModelHealthStatus::InvalidModel,
        ApiError::Api { status, .. } if status.as_u16() == 429 => ModelHealthStatus::RateLimited,
        ApiError::Api { status, .. } if status.is_server_error() => {
            ModelHealthStatus::ProviderUnavailable
        }
        ApiError::Http(error) if error.is_connect() || error.is_timeout() => {
            ModelHealthStatus::ProviderUnavailable
        }
        ApiError::RetriesExhausted { last_error, .. } => classify_error(last_error),
        _ => ModelHealthStatus::RequestFailed,
    }
}

pub async fn probe_model(model: &str) -> ModelHealth {
    let resolved = providers::resolve_model_alias(model);
    let started = Instant::now();
    let client = match ProviderClient::from_model(model) {
        Ok(client) => client,
        Err(error) => {
            return ModelHealth {
                model: resolved,
                status: classify_error(&error),
                latency_ms: None,
                detail: error.to_string(),
            };
        }
    };

    let request = MessageRequest {
        model: resolved.clone(),
        max_tokens: 1,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: "Reply with OK.".to_string(),
            }],
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
    };

    match client.send_message(&request).await {
        Ok(_) => ModelHealth {
            model: resolved,
            status: ModelHealthStatus::Ready,
            latency_ms: Some(started.elapsed().as_millis()),
            detail: "model accepted a minimal completion request".to_string(),
        },
        Err(error) => ModelHealth {
            model: resolved,
            status: classify_error(&error),
            latency_ms: Some(started.elapsed().as_millis()),
            detail: error.to_string(),
        },
    }
}

pub async fn probe_catalog() -> Vec<ModelHealth> {
    let mut results = Vec::with_capacity(MODEL_CATALOG.len());
    for entry in MODEL_CATALOG {
        results.push(probe_model(entry.alias).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::{classify_error, ModelHealthStatus};
    use crate::error::ApiError;

    #[test]
    fn classifies_auth_failures() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            error_type: None,
            message: Some("bad key".to_string()),
            body: String::new(),
            retryable: false,
        };
        assert_eq!(classify_error(&error), ModelHealthStatus::MissingCredentials);
    }

    #[test]
    fn classifies_model_and_rate_limit_failures() {
        let not_found = ApiError::Api {
            status: reqwest::StatusCode::NOT_FOUND,
            error_type: None,
            message: None,
            body: String::new(),
            retryable: false,
        };
        let rate_limited = ApiError::Api {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            error_type: None,
            message: None,
            body: String::new(),
            retryable: true,
        };
        assert_eq!(classify_error(&not_found), ModelHealthStatus::InvalidModel);
        assert_eq!(classify_error(&rate_limited), ModelHealthStatus::RateLimited);
    }

    #[test]
    fn classifies_provider_unavailable() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_GATEWAY,
            error_type: None,
            message: None,
            body: String::new(),
            retryable: true,
        };
        assert_eq!(classify_error(&error), ModelHealthStatus::ProviderUnavailable);
    }
}
