use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
use serde_json::json;

#[test]
fn plugin_tool_receives_json_input_through_environment() {
    let tool = PluginTool::new(
        "input-demo",
        "input-demo",
        PluginToolDefinition {
            name: "plugin_input".to_string(),
            description: Some("input test".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"message": {"type": "string"}}
            }),
        },
        "sh",
        vec![
            "-c".to_string(),
            "printf '%s' \"$CLAW_TOOL_INPUT\"".to_string(),
        ],
        PluginToolPermission::ReadOnly,
        None,
    );

    let output = tool
        .execute(&json!({"message": "hello", "count": 2}))
        .expect("plugin tool should receive and return its JSON input");

    let parsed: serde_json::Value = serde_json::from_str(&output).expect("plugin output should be JSON");
    assert_eq!(parsed, json!({"message": "hello", "count": 2}));
}
