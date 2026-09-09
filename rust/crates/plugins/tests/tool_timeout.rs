use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn plugin_tool_timeout_kills_hanging_process() {
    std::env::set_var("CLAW_PLUGIN_TOOL_TIMEOUT_MS", "100");
    let tool = PluginTool::new(
        "timeout-demo",
        "timeout-demo",
        PluginToolDefinition {
            name: "plugin_timeout".to_string(),
            description: Some("timeout test".to_string()),
            input_schema: json!({"type": "object"}),
        },
        "sh",
        vec!["-c".to_string(), "sleep 1".to_string()],
        PluginToolPermission::ReadOnly,
        None,
    );

    let started = Instant::now();
    let error = tool
        .execute(&json!({}))
        .expect_err("hanging plugin tool should time out");
    let elapsed = started.elapsed();
    std::env::remove_var("CLAW_PLUGIN_TOOL_TIMEOUT_MS");

    assert!(error.to_string().contains("timed out after 100 ms"));
    assert!(elapsed < Duration::from_millis(900));
}
