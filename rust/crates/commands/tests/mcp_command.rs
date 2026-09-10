use commands::{render_mcp_report, SlashCommand};
use runtime::{ConfigSource, McpServerConfig, McpServerType, McpTransport, ScopedMcpServerConfig};
use std::collections::BTreeMap;

#[test]
fn mcp_command_is_parseable_and_rendered_from_shared_config() {
    assert_eq!(SlashCommand::parse("/mcp"), Some(SlashCommand::Mcp));
    let servers = BTreeMap::from([(
        "demo".to_string(),
        ScopedMcpServerConfig {
            scope: ConfigSource::Project,
            config: McpServerConfig::Stdio(runtime::McpStdioServerConfig {
                command: "demo".to_string(),
                args: vec!["--stdio".to_string()],
                env: BTreeMap::new(),
                env_vars: vec![],
            }),
        },
    )]);
    let report = render_mcp_report(&servers);
    assert!(report.contains("demo"));
    assert!(report.contains("stdio"));
    assert!(report.contains("scope=project"));
    assert!(report.contains("demo --stdio"));
}
