use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

use api::{
    read_gemini_base_url, read_nvidia_base_url, read_xai_base_url, ApiError, AuthSource,
    ProviderClient, ProviderKind,
};

#[test]
fn provider_client_routes_grok_aliases_through_xai() {
    let _lock = env_lock();
    let _xai_api_key = EnvVarGuard::set("XAI_API_KEY", Some("xai-test-key"));

    let client = ProviderClient::from_model("grok-mini").expect("grok alias should resolve");

    assert_eq!(client.provider_kind(), ProviderKind::Xai);
}

#[test]
fn provider_client_routes_gemini_aliases_through_gemini() {
    let _lock = env_lock();
    let _gemini_api_key = EnvVarGuard::set("GEMINI_API_KEY", Some("gemini-test-key"));

    let client = ProviderClient::from_model("gemini-flash").expect("gemini alias should resolve");

    assert_eq!(client.provider_kind(), ProviderKind::Gemini);
}

#[test]
fn provider_client_routes_nvidia_aliases_through_nvidia() {
    let _lock = env_lock();
    let _nvidia_api_key = EnvVarGuard::set("NVIDIA_API_KEY", Some("nvidia-test-key"));

    let client = ProviderClient::from_model("nvidia-fast").expect("NVIDIA alias should resolve");

    assert_eq!(client.provider_kind(), ProviderKind::Nvidia);
}

#[test]
fn provider_client_reports_missing_xai_credentials_for_grok_models() {
    let _lock = env_lock();
    let _xai_api_key = EnvVarGuard::set("XAI_API_KEY", None);

    let error = ProviderClient::from_model("grok-3")
        .expect_err("grok requests without XAI_API_KEY should fail fast");

    match error {
        ApiError::MissingCredentials { provider, env_vars } => {
            assert_eq!(provider, "xAI");
            assert_eq!(env_vars, &["XAI_API_KEY"]);
        }
        other => panic!("expected missing xAI credentials, got {other:?}"),
    }
}

#[test]
fn provider_client_reports_missing_gemini_credentials_for_gemini_models() {
    let _lock = env_lock();
    let _gemini_api_key = EnvVarGuard::set("GEMINI_API_KEY", None);

    let error = ProviderClient::from_model("gemini-3.7-flash")
        .expect_err("gemini requests without GEMINI_API_KEY should fail fast");

    match error {
        ApiError::MissingCredentials { provider, env_vars } => {
            assert_eq!(provider, "Gemini");
            assert_eq!(env_vars, &["GEMINI_API_KEY"]);
        }
        other => panic!("expected missing Gemini credentials, got {other:?}"),
    }
}

#[test]
fn provider_client_reports_missing_nvidia_credentials_for_nvidia_models() {
    let _lock = env_lock();
    let _nvidia_api_key = EnvVarGuard::set("NVIDIA_API_KEY", None);

    let error = ProviderClient::from_model("nvidia-fast")
        .expect_err("NVIDIA requests without NVIDIA_API_KEY should fail fast");

    match error {
        ApiError::MissingCredentials { provider, env_vars } => {
            assert_eq!(provider, "NVIDIA");
            assert_eq!(env_vars, &["NVIDIA_API_KEY"]);
        }
        other => panic!("expected missing NVIDIA credentials, got {other:?}"),
    }
}

#[test]
fn provider_client_uses_explicit_auth_without_env_lookup() {
    let _lock = env_lock();
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", None);

    let client = ProviderClient::from_model_with_default_auth(
        "claude-sonnet-4-6",
        Some(AuthSource::ApiKey("claw-test-key".to_string())),
    )
    .expect("explicit auth should avoid env lookup");

    assert_eq!(client.provider_kind(), ProviderKind::ClawApi);
}

#[test]
fn read_gemini_base_url_prefers_env_override() {
    let _lock = env_lock();
    let _gemini_base_url = EnvVarGuard::set(
        "GEMINI_BASE_URL",
        Some("https://example.gemini.test/v1beta"),
    );

    assert_eq!(read_gemini_base_url(), "https://example.gemini.test/v1beta");
}

#[test]
fn read_xai_base_url_prefers_env_override() {
    let _lock = env_lock();
    let _xai_base_url = EnvVarGuard::set("XAI_BASE_URL", Some("https://example.xai.test/v1"));

    assert_eq!(read_xai_base_url(), "https://example.xai.test/v1");
}

#[test]
fn read_nvidia_base_url_prefers_env_override() {
    let _lock = env_lock();
    let _nvidia_base_url =
        EnvVarGuard::set("NVIDIA_BASE_URL", Some("https://example.nvidia.test/v1"));

    assert_eq!(read_nvidia_base_url(), "https://example.nvidia.test/v1");
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let original = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
