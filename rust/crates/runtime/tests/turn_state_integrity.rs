use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, MessageRole, PermissionMode,
    PermissionPolicy, RuntimeError, Session, StaticToolExecutor,
};

struct ProviderFailsAfterTool {
    calls: usize,
}

impl ApiClient for ProviderFailsAfterTool {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        match self.calls {
            1 => {
                assert_eq!(request.messages.len(), 1);
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "echo".to_string(),
                        input: "hello".to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
            2 => {
                let last = request.messages.last().expect("tool result should be present");
                assert_eq!(last.role, MessageRole::Tool);
                Err(RuntimeError::new("provider unavailable"))
            }
            _ => unreachable!("runtime should stop after the provider error"),
        }
    }
}

#[test]
fn keeps_completed_tool_state_when_the_next_provider_call_fails() {
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        ProviderFailsAfterTool { calls: 0 },
        StaticToolExecutor::new().register("echo", |input| Ok(input.to_string())),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
    );

    let error = runtime
        .run_turn("say hello", None)
        .expect_err("provider failure should be returned");
    assert_eq!(error.to_string(), "provider unavailable");

    assert_eq!(runtime.session().messages.len(), 3);
    assert!(matches!(runtime.session().messages[0].role, MessageRole::User));
    assert!(matches!(runtime.session().messages[1].role, MessageRole::Assistant));
    assert!(matches!(runtime.session().messages[2].role, MessageRole::Tool));
}

struct FinalizesAtToolLimitApi {
    calls: usize,
}

impl ApiClient for FinalizesAtToolLimitApi {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        if request
            .system_prompt
            .iter()
            .any(|section| section.contains("FINALIZATION PASS"))
        {
            return Ok(vec![
                AssistantEvent::TextDelta(
                    "Completed: two tool iterations.\nRemaining: none known.\nValidation: tool results were collected.\nBlockers: none reported.".to_string(),
                ),
                AssistantEvent::MessageStop,
            ]);
        }

        Ok(vec![
            AssistantEvent::ToolUse {
                id: format!("tool-{}", self.calls),
                name: "echo".to_string(),
                input: "continue".to_string(),
            },
            AssistantEvent::MessageStop,
        ])
    }
}

#[test]
fn iteration_limit_enters_finalization_instead_of_returning_an_error() {
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        FinalizesAtToolLimitApi { calls: 0 },
        StaticToolExecutor::new().register("echo", |input| Ok(input.to_string())),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
    )
    .with_max_iterations(2);

    let summary = runtime
        .run_turn("keep going", None)
        .expect("iteration limit should hand off to finalization");

    assert_eq!(summary.iterations, 3);
    assert!(summary
        .assistant_messages
        .last()
        .expect("final assistant message")
        .blocks
        .iter()
        .any(|block| matches!(
            block,
            runtime::ContentBlock::Text { text } if text.contains("Completed:")
        )));
}

struct AlwaysRequestsToolApi {
    calls: usize,
}

impl ApiClient for AlwaysRequestsToolApi {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        assert!(request.messages.len() >= self.calls);
        Ok(vec![
            AssistantEvent::ToolUse {
                id: format!("tool-{}", self.calls),
                name: "echo".to_string(),
                input: "continue".to_string(),
            },
            AssistantEvent::MessageStop,
        ])
    }
}

#[test]
fn repeated_tool_requests_during_finalization_are_not_executed() {
    let mut executions = 0;
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        AlwaysRequestsToolApi { calls: 0 },
        StaticToolExecutor::new().register("echo", move |input| {
            executions += 1;
            Ok(input.to_string())
        }),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
    )
    .with_max_iterations(1);

    let summary = runtime
        .run_turn("keep going", None)
        .expect("bounded finalization should return without executing finalization tools");

    assert_eq!(summary.iterations, 4);
    assert_eq!(executions, 1);
}
