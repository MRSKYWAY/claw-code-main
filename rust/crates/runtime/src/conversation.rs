use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};

use crate::cancellation::CancellationToken;
use crate::compact::{
    compact_session, estimate_session_tokens, CompactionConfig, CompactionResult,
};
use crate::config::RuntimeFeatureConfig;
use crate::decision::{JevDecisionProvider, ToolDecision};
use crate::hooks::{HookRunResult, HookRunner};
use crate::permissions::{
    PermissionOutcome, PermissionPolicy, PermissionPrompter, PermissionRequest, ToolPolicyDecision,
};
use crate::scope::apply_user_scope_constraints;
use crate::session::{ContentBlock, ConversationMessage, Session};
use crate::usage::{TokenUsage, UsageTracker};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequest {
    pub system_prompt: Vec<String>,
    pub messages: Vec<ConversationMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantEvent {
    TextDelta(String),
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    Usage(TokenUsage),
    MessageStop,
}

pub trait ApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError>;

    fn stream_with_cancellation(&mut self, request: ApiRequest, cancellation: &CancellationToken) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::new("conversation turn cancelled"));
        }
        self.stream(request)
    }
}

pub trait ToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.message) }
}
impl std::error::Error for ToolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError { message: String }
impl RuntimeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self { Self { message: message.into() } }
}
impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.message) }
}
impl std::error::Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub assistant_messages: Vec<ConversationMessage>,
    pub tool_results: Vec<ConversationMessage>,
    pub iterations: usize,
    pub usage: TokenUsage,
}

const MAX_FINALIZATION_ATTEMPTS: usize = 3;
const FINALIZATION_SYSTEM_INSTRUCTION: &str = "FINALIZATION PASS: The configured tool-iteration budget has been reached. Do not use any more tools or modify the workspace. Based only on the work already completed in this conversation, provide the final response now. Use these headings exactly: Completed, Remaining, Validation, Blockers. State clearly what was changed, what remains unfinished, what validation succeeded or could not be completed, and any known blocker. Do not claim completion for work you could not verify.";
const FINALIZATION_TOOL_BLOCK_MESSAGE: &str = "The tool-iteration budget has been reached, so tools are disabled for this finalization pass. Stop calling tools and provide the requested final response with Completed, Remaining, Validation, and Blockers.";
const FINALIZATION_FALLBACK_MESSAGE: &str = "Completed: the configured tool-iteration budget was consumed and completed tool results were preserved.\nRemaining: the model did not provide a final synthesis, so the exact remaining work could not be confirmed.\nValidation: all tool results produced before finalization remain in the session.\nBlockers: the model continued requesting tools during the bounded finalization pass.";
const EMPTY_STREAM_MESSAGE: &str = "assistant stream produced no content";

pub struct ConversationRuntime<C, T> {
    session: Session,
    api_client: C,
    tool_executor: T,
    permission_policy: PermissionPolicy,
    system_prompt: Vec<String>,
    max_iterations: usize,
    usage_tracker: UsageTracker,
    hook_runner: HookRunner,
    jev_provider: Option<JevDecisionProvider>,
}

impl<C, T> ConversationRuntime<C, T>
where
    C: ApiClient,
    T: ToolExecutor,
{
    #[must_use]
    pub fn new(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<String>,
    ) -> Self {
        Self::new_with_features(
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            &RuntimeFeatureConfig::default(),
        )
    }

    #[must_use]
    pub fn new_with_features(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<String>,
        feature_config: &RuntimeFeatureConfig,
    ) -> Self {
        let usage_tracker = UsageTracker::from_session(&session);
        let jev_provider = feature_config
            .jev()
            .enabled()
            .then(|| JevDecisionProvider::from_config(feature_config.jev()));
        Self {
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            max_iterations: 8,
            usage_tracker,
            hook_runner: HookRunner::from_feature_config(feature_config),
            jev_provider,
        }
    }

    #[must_use]
    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn run_turn(
        &mut self,
        user_input: impl Into<String>,
        prompter: Option<&mut dyn PermissionPrompter>,
    ) -> Result<TurnSummary, RuntimeError> {
        self.run_turn_with_cancellation(user_input, prompter, &CancellationToken::new())
    }

    pub fn run_turn_with_cancellation(
        &mut self,
        user_input: impl Into<String>,
        mut prompter: Option<&mut dyn PermissionPrompter>,
        cancellation: &CancellationToken,
    ) -> Result<TurnSummary, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::new("conversation turn cancelled"));
        }

        let user_input = user_input.into();
        let newly_excluded = apply_user_scope_constraints(&mut self.session, &user_input);
        let _ = newly_excluded;
        self.session
            .messages
            .push(ConversationMessage::user_text(user_input));

        let mut assistant_messages = Vec::new();
        let mut tool_results = Vec::new();
        let mut iterations = 0;
        let mut finalization_mode = false;
        let mut finalization_attempts = 0;

        loop {
            iterations += 1;
            if !finalization_mode && iterations > self.max_iterations {
                finalization_mode = true;
                finalization_attempts = 0;
            }

            if finalization_mode {
                finalization_attempts += 1;
                if finalization_attempts > MAX_FINALIZATION_ATTEMPTS {
                    let fallback_message = ConversationMessage::assistant(vec![ContentBlock::Text {
                        text: FINALIZATION_FALLBACK_MESSAGE.to_string(),
                    }]);
                    self.session.messages.push(fallback_message.clone());
                    assistant_messages.push(fallback_message);
                    break;
                }
            }

            if cancellation.is_cancelled() {
                return Err(RuntimeError::new("conversation turn cancelled"));
            }

            let mut system_prompt = self.system_prompt.clone();
            if finalization_mode {
                system_prompt.push(FINALIZATION_SYSTEM_INSTRUCTION.to_string());
            }
            let request = ApiRequest {
                system_prompt,
                messages: self.session.messages.clone(),
            };
            let events = self.api_client.stream_with_cancellation(request, cancellation)?;
            let (assistant_message, usage) = match build_assistant_message(events) {
                Ok(result) => result,
                Err(error) if error.to_string() == EMPTY_STREAM_MESSAGE => {
                    if finalization_mode {
                        let fallback_message = ConversationMessage::assistant(vec![ContentBlock::Text {
                            text: "Completed: the model completed tool execution but returned an empty final response.\nRemaining: the exact remaining work could not be determined from the final model response.\nValidation: completed tool results were preserved in the session.\nBlockers: the provider returned an empty assistant stream during finalization.".to_string(),
                        }]);
                        self.session.messages.push(fallback_message.clone());
                        assistant_messages.push(fallback_message);
                        break;
                    }
                    finalization_mode = true;
                    finalization_attempts = 0;
                    continue;
                }
                Err(error) => return Err(error),
            };
            validate_assistant_tool_uses(&assistant_message)?;
            if let Some(usage) = usage {
                self.usage_tracker.record(usage);
            }
            let pending_tool_uses = assistant_message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();

            self.session.messages.push(assistant_message.clone());
            assistant_messages.push(assistant_message);

            if pending_tool_uses.is_empty() {
                break;
            }

            if finalization_mode {
                for (tool_use_id, tool_name, _input) in pending_tool_uses {
                    let result_message = ConversationMessage::tool_result(
                        tool_use_id,
                        tool_name,
                        FINALIZATION_TOOL_BLOCK_MESSAGE,
                        true,
                    );
                    self.session.messages.push(result_message.clone());
                    tool_results.push(result_message);
                }
                continue;
            }

            for (tool_use_id, tool_name, input) in pending_tool_uses {
                if cancellation.is_cancelled() {
                    return Err(RuntimeError::new("conversation turn cancelled"));
                }
                let permission_outcome = if let Some(prompt) = prompter.as_mut() {
                    self.permission_policy
                        .authorize(&tool_name, &input, Some(*prompt))
                } else {
                    self.permission_policy.authorize(&tool_name, &input, None)
                };

                let pre_hook_result = if matches!(&permission_outcome, PermissionOutcome::Allow) {
                    self.hook_runner.run_pre_tool_use(&tool_name, &input)
                } else {
                    HookRunResult::allow(Vec::new())
                };
                let policy_decision = ToolPolicyDecision::from_permission_and_hook(
                    &permission_outcome,
                    &pre_hook_result,
                );

                let workspace = std::env::current_dir()
                    .ok()
                    .map(|path| path.display().to_string());

                let policy_decision = if matches!(&policy_decision, ToolPolicyDecision::Allow) {
                    match &self.jev_provider {
                        None => policy_decision,
                        Some(jev) => match jev.assess_tool(
                            &tool_name,
                            &input,
                            workspace.as_deref(),
                        ) {
                            Ok(ToolDecision::Allow) => ToolPolicyDecision::Allow,
                            Ok(ToolDecision::Confirm) => {
                                let decision = prompter.as_mut().map(|prompt| {
                                    prompt.decide(&PermissionRequest {
                                        tool_name: tool_name.clone(),
                                        input: input.clone(),
                                        current_mode: self.permission_policy.active_mode(),
                                        required_mode: self
                                            .permission_policy
                                            .required_mode_for(&tool_name),
                                    })
                                });
                                match decision {
                                    Some(crate::permissions::PermissionPromptDecision::Allow) => {
                                        ToolPolicyDecision::Allow
                                    }
                                    Some(crate::permissions::PermissionPromptDecision::Deny { reason }) => {
                                        ToolPolicyDecision::PermissionDenied { reason }
                                    }
                                    None => ToolPolicyDecision::PermissionDenied {
                                        reason: "Jev requested confirmation, but no permission prompter is available".to_string(),
                                    },
                                }
                            }
                            Ok(ToolDecision::Deny) => ToolPolicyDecision::PermissionDenied {
                                reason: "Jev denied automatic execution of this tool call".to_string(),
                            },
                            Err(error) => ToolPolicyDecision::PermissionDenied {
                                reason: format!(
                                    "Jev guard failed; tool execution was blocked: {error}"
                                ),
                            },
                        },
                    }
                } else {
                    policy_decision
                };

                let result_message = match policy_decision {
                    ToolPolicyDecision::Allow => {
                        let scoped_input =
                            inject_session_scope_into_tool_input(&tool_name, &input, &self.session);
                        let (mut output, mut is_error) =
                            match self.tool_executor.execute(&tool_name, &scoped_input) {
                                Ok(output) => (output, false),
                                Err(error) => (error.to_string(), true),
                            };
                        output = merge_hook_feedback(pre_hook_result.messages(), output, false);

                        let post_hook_result = self
                            .hook_runner
                            .run_post_tool_use(&tool_name, &input, &output, is_error);
                        if post_hook_result.is_denied() {
                            is_error = true;
                        }
                        output = merge_hook_feedback(
                            post_hook_result.messages(),
                            output,
                            post_hook_result.is_denied(),
                        );

                        ConversationMessage::tool_result(
                            tool_use_id,
                            tool_name,
                            output,
                            is_error,
                        )
                    }
                    ToolPolicyDecision::PermissionDenied { reason } => {
                        ConversationMessage::tool_result(tool_use_id, tool_name, reason, true)
                    }
                    ToolPolicyDecision::HookDenied { messages } => {
                        let deny_message = format!("PreToolUse hook denied tool `{tool_name}`");
                        let output = if messages.is_empty() {
                            deny_message
                        } else {
                            messages.join("\n")
                        };
                        ConversationMessage::tool_result(tool_use_id, tool_name, output, true)
                    }
                };
                self.session.messages.push(result_message.clone());
                tool_results.push(result_message);
            }
        }

        Ok(TurnSummary {
            assistant_messages,
            tool_results,
            iterations,
            usage: self.usage_tracker.cumulative_usage(),
        })
    }

    #[must_use]
    pub fn compact(&self, config: CompactionConfig) -> CompactionResult {
        compact_session(&self.session, config)
    }

    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        estimate_session_tokens(&self.session)
    }

    #[must_use]
    pub fn usage(&self) -> &UsageTracker {
        &self.usage_tracker
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    #[must_use]
    pub fn into_session(self) -> Session {
        self.session
    }
}

fn inject_session_scope_into_tool_input(
    tool_name: &str,
    input: &str,
    session: &Session,
) -> String {
    const SCOPED_TOOLS: &[&str] = &[
        "read_file",
        "write_file",
        "edit_file",
        "glob_search",
        "grep_search",
    ];

    if session.excluded_paths().is_empty() || !SCOPED_TOOLS.contains(&tool_name) {
        return input.to_string();
    }

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return input.to_string();
    };
    object.insert(
        "session_excluded_paths".to_string(),
        serde_json::Value::Array(
            session
                .excluded_paths()
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    serde_json::to_string(&value).unwrap_or_else(|_| input.to_string())
}

fn build_assistant_message(
    events: Vec<AssistantEvent>,
) -> Result<(ConversationMessage, Option<TokenUsage>), RuntimeError> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut finished = false;
    let mut usage = None;

    for event in events {
        match event {
            AssistantEvent::TextDelta(delta) => text.push_str(&delta),
            AssistantEvent::ToolUse { id, name, input } => {
                flush_text_block(&mut text, &mut blocks);
                blocks.push(ContentBlock::ToolUse { id, name, input });
            }
            AssistantEvent::Usage(value) => usage = Some(value),
            AssistantEvent::MessageStop => {
                finished = true;
            }
        }
    }

    flush_text_block(&mut text, &mut blocks);

    if !finished {
        return Err(RuntimeError::new(
            "assistant stream ended without a message stop event",
        ));
    }
    if blocks.is_empty() {
        return Err(RuntimeError::new(EMPTY_STREAM_MESSAGE));
    }

    Ok((
        ConversationMessage::assistant_with_usage(blocks, usage),
        usage,
    ))
}

fn validate_assistant_tool_uses(message: &ConversationMessage) -> Result<(), RuntimeError> {
    let mut tool_ids = HashSet::new();

    for block in &message.blocks {
        let ContentBlock::ToolUse { id, name, .. } = block else {
            continue;
        };

        if id.trim().is_empty() {
            return Err(RuntimeError::new("assistant tool call has an empty id"));
        }
        if name.trim().is_empty() {
            return Err(RuntimeError::new("assistant tool call has an empty name"));
        }
        if !tool_ids.insert(id) {
            return Err(RuntimeError::new(format!(
                "assistant tool call id is duplicated: {id}"
            )));
        }
    }

    Ok(())
}

fn flush_text_block(text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(text),
        });
    }
}

fn format_hook_message(result: &HookRunResult, fallback: &str) -> String {
    if result.messages().is_empty() {
        fallback.to_string()
    } else {
        result.messages().join("\n")
    }
}

fn merge_hook_feedback(messages: &[String], output: String, denied: bool) -> String {
    if messages.is_empty() {
        return output;
    }

    let mut sections = Vec::new();
    if !output.trim().is_empty() {
        sections.push(output);
    }
    let label = if denied {
        "Hook feedback (denied)"
    } else {
        "Hook feedback"
    };
    sections.push(format!("{label}:\n{}", messages.join("\n")));
    sections.join("\n\n")
}

type ToolHandler = Box<dyn FnMut(&str) -> Result<String, ToolError>>;

#[derive(Default)]
pub struct StaticToolExecutor {
    handlers: BTreeMap<String, ToolHandler>,
}

impl StaticToolExecutor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn register(
        mut self,
        tool_name: impl Into<String>,
        handler: impl FnMut(&str) -> Result<String, ToolError> + 'static,
    ) -> Self {
        self.handlers.insert(tool_name.into(), Box::new(handler));
        self
    }
}

impl ToolExecutor for StaticToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        self.handlers
            .get_mut(tool_name)
            .ok_or_else(|| ToolError::new(format!("unknown tool: {tool_name}")))?(input)
    }
}
