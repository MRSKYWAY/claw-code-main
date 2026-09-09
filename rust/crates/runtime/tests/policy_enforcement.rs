use runtime::{
    ApiClient, ApiRequest, AssistantEvent, CompactionConfig, ContentBlock, ConversationMessage,
    ConversationRuntime, MessageRole, PermissionMode, PermissionOutcome, PermissionPolicy,
    PermissionPromptDecision, PermissionPrompter, PermissionRequest, ProjectContext, RuntimeError,
    RuntimeFeatureConfig, RuntimeHookConfig, Session, StaticToolExecutor, SystemPromptBuilder,
    TokenUsage,
};
use std::path::PathBuf;

struct ScriptedApiClient {
    call_count: usize,
}

impl ApiClient for ScriptedApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.call_count += 1;
        match self.call_count {
            1 => {
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::User));
                Ok(vec![
                    AssistantEvent::TextDelta("Let me calculate that.".to_string()),
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "add".to_string(),
                        input: "2,2".to_string(),
                    },
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 20,
                        output_tokens: 6,
                        cache_creation_input_tokens: 1,
                        cache_read_input_tokens: 2,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
            2 => {
                let last_message = request
                    .messages
                    .last()
                    .expect("tool result should be present");
                assert_eq!(last_message.role, MessageRole::Tool);
                Ok(vec![
                    AssistantEvent::TextDelta("The answer is 4.".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 24,
                        output_tokens: 4,
                        cache_creation_input_tokens: 1,
                        cache_read_input_tokens: 3,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
            _ => Err(RuntimeError::new("unexpected extra API call")),
        }
    }
}

struct PromptAllowOnce;
impl PermissionPrompter for PromptAllowOnce {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        assert_eq!(request.tool_name, "add");
        PermissionPromptDecision::Allow
    }
}

#[test]
fn runs_user_to_tool_to_result_loop_end_to_end_and_tracks_usage() {
    let api_client = ScriptedApiClient { call_count: 0 };
    let tool_executor = StaticToolExecutor::new().register("add", |input| {
        let total = input
            .split(',')
            .map(|part| part.parse::<i32>().expect("input must be valid integer"))
            .sum::<i32>();
        Ok(total.to_string())
    });
    let permission_policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite);
    let system_prompt = SystemPromptBuilder::new()
        .with_project_context(ProjectContext {
            cwd: PathBuf::from("/tmp/project"),
            current_date: "2026-03-31".to_string(),
            git_status: None,
            git_diff: None,
            instruction_files: Vec::new(),
        })
        .with_os("linux", "6.8")
        .build();
    let mut runtime = ConversationRuntime::new(
        Session::new(), api_client, tool_executor, permission_policy, system_prompt,
    );
    let summary = runtime
        .run_turn("what is 2 + 2?", Some(&mut PromptAllowOnce))
        .expect("conversation loop should succeed");
    assert_eq!(summary.iterations, 2);
    assert_eq!(summary.assistant_messages.len(), 2);
    assert_eq!(summary.tool_results.len(), 1);
    assert_eq!(runtime.session().messages.len(), 4);
    assert_eq!(summary.usage.output_tokens, 10);
    assert!(matches!(runtime.session().messages[1].blocks[1], ContentBlock::ToolUse { .. }));
    assert!(matches!(runtime.session().messages[2].blocks[0], ContentBlock::ToolResult { is_error: false, .. }));
}

#[test]
fn rejects_malformed_tool_calls_before_history_mutation() {
    struct MalformedApi;
    impl ApiClient for MalformedApi {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            let count = request.messages.len();
            let (id, name) = match count {
                1 => ("   ".to_string(), "bash".to_string()),
                _ => ("tool-1".to_string(), "".to_string()),
            };
            Ok(vec![AssistantEvent::ToolUse { id, name, input: "echo hi".to_string() }, AssistantEvent::MessageStop])
        }
    }
    let mut runtime = ConversationRuntime::new(
        Session::new(), MalformedApi, StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()],
    );
    let error = runtime.run_turn("use the tool", None).expect_err("invalid tool call");
    assert!(error.to_string().contains("empty id") || error.to_string().contains("empty name"));
    assert_eq!(runtime.session().messages.len(), 1);
}

#[test]
fn rejects_duplicate_tool_ids_before_history_mutation() {
    struct MalformedApi;
    impl ApiClient for MalformedApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            Ok(vec![
                AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "first".to_string(), input: "one".to_string() },
                AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "second".to_string(), input: "two".to_string() },
                AssistantEvent::MessageStop,
            ])
        }
    }
    let mut runtime = ConversationRuntime::new(
        Session::new(), MalformedApi, StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()],
    );
    let error = runtime.run_turn("use both", None).expect_err("duplicate id");
    assert_eq!(error.to_string(), "assistant tool call id is duplicated: tool-1");
    assert_eq!(runtime.session().messages.len(), 1);
}

#[test]
fn records_denied_tool_results_when_prompt_rejects() {
    struct RejectPrompter;
    impl PermissionPrompter for RejectPrompter {
        fn decide(&mut self, _request: &PermissionRequest) -> PermissionPromptDecision {
            PermissionPromptDecision::Deny { reason: "not now".to_string() }
        }
    }
    struct SingleCallApi;
    impl ApiClient for SingleCallApi {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            if request.messages.iter().any(|m| m.role == MessageRole::Tool) {
                return Ok(vec![AssistantEvent::TextDelta("done".to_string()), AssistantEvent::MessageStop]);
            }
            Ok(vec![AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "blocked".to_string(), input: "secret".to_string() }, AssistantEvent::MessageStop])
        }
    }
    let mut runtime = ConversationRuntime::new(
        Session::new(), SingleCallApi, StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::WorkspaceWrite), vec!["system".to_string()],
    );
    let summary = runtime.run_turn("use the tool", Some(&mut RejectPrompter)).expect("turn should continue");
    assert!(matches!(&summary.tool_results[0].blocks[0], ContentBlock::ToolResult { is_error: true, output, .. } if output == "not now"));
}

#[test]
fn permission_denial_short_circuits_pre_tool_hook_and_tool() {
    struct SingleCallApi;
    impl ApiClient for SingleCallApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            Ok(vec![AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "blocked".to_string(), input: "secret".to_string() }, AssistantEvent::MessageStop])
        }
    }
    let mut runtime = ConversationRuntime::new_with_features(
        Session::new(), SingleCallApi,
        StaticToolExecutor::new().register("blocked", |_input| panic!("tool must not execute after permission denial")),
        PermissionPolicy::new(PermissionMode::ReadOnly).with_tool_requirement("blocked", PermissionMode::WorkspaceWrite),
        vec!["system".to_string()],
        &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(vec!["printf 'hook denial'; exit 2".to_string()], Vec::new())),
    );
    let summary = runtime.run_turn("use the tool", None).expect("permission denial should remain a tool result");
    let ContentBlock::ToolResult { is_error, output, .. } = &summary.tool_results[0].blocks[0] else { panic!("expected tool result") };
    assert!(*is_error);
    assert!(output.contains("requires workspace-write permission"));
    assert!(!output.contains("hook denial"));
    assert_eq!(runtime.session().messages.len(), 3);
}

#[test]
fn denies_tool_use_when_pre_tool_hook_blocks() {
    struct SingleCallApi;
    impl ApiClient for SingleCallApi {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            if request.messages.iter().any(|m| m.role == MessageRole::Tool) {
                return Ok(vec![AssistantEvent::TextDelta("blocked".to_string()), AssistantEvent::MessageStop]);
            }
            Ok(vec![AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "blocked".to_string(), input: "secret".to_string() }, AssistantEvent::MessageStop])
        }
    }
    let mut runtime = ConversationRuntime::new_with_features(
        Session::new(), SingleCallApi,
        StaticToolExecutor::new().register("blocked", |_input| panic!("tool should not execute when hook denies")),
        PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()],
        &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(vec!["printf 'blocked by hook'; exit 2".to_string()], Vec::new())),
    );
    let summary = runtime.run_turn("use the tool", None).expect("hook denial should remain a tool result");
    let ContentBlock::ToolResult { is_error, output, .. } = &summary.tool_results[0].blocks[0] else { panic!("expected tool result") };
    assert!(*is_error);
    assert!(output.contains("denied tool") || output.contains("blocked by hook"));
}

#[test]
fn appends_post_tool_hook_feedback_to_tool_result() {
    struct TwoCallApi { calls: usize }
    impl ApiClient for TwoCallApi {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            match self.calls {
                1 => Ok(vec![AssistantEvent::ToolUse { id: "tool-1".to_string(), name: "add".to_string(), input: "2,2".to_string() }, AssistantEvent::MessageStop]),
                2 => { assert!(request.messages.iter().any(|m| m.role == MessageRole::Tool)); Ok(vec![AssistantEvent::TextDelta("done".to_string()), AssistantEvent::MessageStop]) }
                _ => Err(RuntimeError::new("unexpected extra API call")),
            }
        }
    }
    let mut runtime = ConversationRuntime::new_with_features(
        Session::new(), TwoCallApi { calls: 0 }, StaticToolExecutor::new().register("add", |_input| Ok("4".to_string())),
        PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()],
        &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(vec!["printf 'pre hook ran'".to_string()], vec!["printf 'post hook ran'".to_string()])),
    );
    let summary = runtime.run_turn("use add", None).expect("tool loop succeeds");
    let ContentBlock::ToolResult { is_error, output, .. } = &summary.tool_results[0].blocks[0] else { panic!("expected tool result") };
    assert!(!*is_error);
    assert!(output.contains('4'));
    assert!(output.contains("pre hook ran"));
    assert!(output.contains("post hook ran"));
}

#[test]
fn reconstructs_usage_tracker_from_restored_session() {
    struct SimpleApi;
    impl ApiClient for SimpleApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            Ok(vec![AssistantEvent::TextDelta("done".to_string()), AssistantEvent::MessageStop])
        }
    }
    let mut session = Session::new();
    session.messages.push(ConversationMessage::assistant_with_usage(vec![ContentBlock::Text { text: "earlier".to_string() }], Some(TokenUsage { input_tokens: 11, output_tokens: 7, cache_creation_input_tokens: 2, cache_read_input_tokens: 1 })));
    let runtime = ConversationRuntime::new(session, SimpleApi, StaticToolExecutor::new(), PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()]);
    assert_eq!(runtime.usage().turns(), 1);
    assert_eq!(runtime.usage().cumulative_usage().total_tokens(), 21);
}

#[test]
fn compacts_session_after_turns() {
    struct SimpleApi;
    impl ApiClient for SimpleApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            Ok(vec![AssistantEvent::TextDelta("done".to_string()), AssistantEvent::MessageStop])
        }
    }
    let mut runtime = ConversationRuntime::new(Session::new(), SimpleApi, StaticToolExecutor::new(), PermissionPolicy::new(PermissionMode::DangerFullAccess), vec!["system".to_string()]);
    runtime.run_turn("a", None).expect("turn a");
    runtime.run_turn("b", None).expect("turn b");
    runtime.run_turn("c", None).expect("turn c");
    let result = runtime.compact(CompactionConfig { preserve_recent_messages: 2, max_estimated_tokens: 1 });
    assert!(result.summary.contains("Conversation summary"));
    assert_eq!(result.compacted_session.messages[0].role, MessageRole::System);
}
