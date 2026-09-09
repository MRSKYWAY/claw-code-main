use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, PermissionMode, PermissionPolicy,
    RuntimeError, RuntimeFeatureConfig, RuntimeHookConfig, StaticToolExecutor,
};
use runtime::session::{ContentBlock, Session};

struct SingleCallApi;

impl ApiClient for SingleCallApi {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        Ok(vec![
            AssistantEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "blocked".to_string(),
                input: "secret".to_string(),
            },
            AssistantEvent::MessageStop,
        ])
    }
}

#[test]
fn permission_denial_short_circuits_pre_tool_hook_and_tool() {
    let mut runtime = ConversationRuntime::new_with_features(
        Session::new(),
        SingleCallApi,
        StaticToolExecutor::new().register("blocked", |_input| {
            panic!("tool must not execute after permission denial")
        }),
        PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("blocked", PermissionMode::WorkspaceWrite),
        vec!["system".to_string()],
        &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
            vec!["printf 'hook denial'; exit 2".to_string()],
            Vec::new(),
        )),
    );

    let summary = runtime
        .run_turn("use the tool", None)
        .expect("permission denial should remain a tool result");

    let ContentBlock::ToolResult { is_error, output, .. } = &summary.tool_results[0].blocks[0]
    else {
        panic!("expected tool result block");
    };
    assert!(*is_error);
    assert!(output.contains("requires workspace-write permission"));
    assert!(!output.contains("hook denial"));
    assert_eq!(runtime.session().messages.len(), 3);
}
