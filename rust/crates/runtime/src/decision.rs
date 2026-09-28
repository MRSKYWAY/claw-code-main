use std::collections::BTreeMap;
use std::env;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};


use crate::config::RuntimeJevConfig;

const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1_500);
const ALLOW_THRESHOLD: f64 = 0.85;
const CONFIRM_THRESHOLD: f64 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDecision {
    Allow,
    Confirm,
    Deny,
}

#[derive(Debug)]
pub enum DecisionError {
    MissingCredentials,
    Request(reqwest::Error),
    Http { status: reqwest::StatusCode, body: String },
    InvalidResponse(String),
}

impl Display for DecisionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials => {
                write!(f, "TYPESAFE_API_KEY is required when the Jev guard is enabled")
            }
            Self::Request(error) => write!(f, "Jev request failed: {error}"),
            Self::Http { status, body } => {
                write!(f, "Jev returned HTTP {status}: {}", body.trim())
            }
            Self::InvalidResponse(message) => write!(f, "invalid Jev response: {message}"),
        }
    }
}

impl std::error::Error for DecisionError {}

#[derive(Debug, Clone)]
pub struct JevDecisionProvider {
    client: Client,
    api_key: Option<String>,
    model: String,
    endpoint: String,
}

#[derive(Debug, Serialize)]
struct JevRequest<'a> {
    model: &'a str,
    state: String,
    questions: JevQuestions,
}

#[derive(Debug, Serialize)]
struct JevQuestions {
    allow: JevNoulQuestion,
}

#[derive(Debug, Serialize)]
struct JevNoulQuestion {
    #[serde(rename = "type")]
    question_type: &'static str,
    instructions: &'static str,
}

#[derive(Debug, Deserialize)]
struct JevResponse {
    answers: BTreeMap<String, JevAnswer>,
}

#[derive(Debug, Deserialize)]
struct JevAnswer {
    noul: Option<f64>,
}

impl JevDecisionProvider {
    #[must_use]
    pub fn from_config(config: &RuntimeJevConfig) -> Self {
        let timeout = DEFAULT_TIMEOUT;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest blocking client should build with a fixed timeout");

        let base_url = env::var("TYPESAFE_API_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        Self {
            client,
            api_key: env::var("TYPESAFE_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            model: config.model().to_string(),
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
        }
    }

    pub fn assess_tool(
        &self,
        tool_name: &str,
        tool_input: &str,
        workspace: Option<&str>,
    ) -> Result<ToolDecision, DecisionError> {
        let Some(api_key) = self.api_key.as_deref() else {
            return Err(DecisionError::MissingCredentials);
        };

        let workspace = workspace.unwrap_or("<unknown>");
        let state = format!(
            "Claw Code is deciding whether to execute an AI-requested tool call automatically.\n             Workspace: {workspace}\n             Tool: {tool_name}\n             Tool input:\n{tool_input}"
        );

        let request = JevRequest {
            model: &self.model,
            state,
            questions: JevQuestions {
                allow: JevNoulQuestion {
                    question_type: "noul",
                    instructions:
                        "Should this tool call be allowed to execute automatically? Consider destructive actions, credential or secret exposure, remote side effects, persistence, and blast radius. Answer yes only when automatic execution is appropriate.",
                },
            },
        };

        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .map_err(DecisionError::Request)?;

        let status = response.status();
        let body = response.text().map_err(DecisionError::Request)?;

        if !status.is_success() {
            return Err(DecisionError::Http { status, body });
        }

        let parsed = serde_json::from_str::<JevResponse>(&body)
            .map_err(|error| DecisionError::InvalidResponse(error.to_string()))?;

        let probability = parsed
            .answers
            .get("allow")
            .and_then(|answer| answer.noul)
            .ok_or_else(|| {
                DecisionError::InvalidResponse(
                    "answers.allow.noul was missing from the response".to_string(),
                )
            })?;

        if !(0.0..=1.0).contains(&probability) {
            return Err(DecisionError::InvalidResponse(format!(
                "answers.allow.noul must be between 0 and 1, got {probability}"
            )));
        }

        Ok(classify_tool_probability(probability))
    }
}

#[must_use]
pub fn classify_tool_probability(probability: f64) -> ToolDecision {
    if probability >= ALLOW_THRESHOLD {
        ToolDecision::Allow
    } else if probability >= CONFIRM_THRESHOLD {
        ToolDecision::Confirm
    } else {
        ToolDecision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_tool_probability, ToolDecision};

    #[test]
    fn classifies_jev_probability_into_deterministic_actions() {
        assert_eq!(classify_tool_probability(0.95), ToolDecision::Allow);
        assert_eq!(classify_tool_probability(0.85), ToolDecision::Allow);
        assert_eq!(classify_tool_probability(0.60), ToolDecision::Confirm);
        assert_eq!(classify_tool_probability(0.45), ToolDecision::Confirm);
        assert_eq!(classify_tool_probability(0.10), ToolDecision::Deny);
    }
}
