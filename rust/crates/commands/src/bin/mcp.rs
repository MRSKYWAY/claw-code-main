use std::env;
use std::fmt::Write as _;

use runtime::{ConfigLoader, ConfigSource, McpServerConfig, McpTransport, ScopedMcpServerConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    println!("{}", render_mcp_report(config.mcp().servers()));
    Ok(())
}

fn render_mcp_report(
    servers: &std::collections::BTreeMap<String, ScopedMcpServerConfig>,
) -> String {
    let mut lines = vec![
        "MCP servers".to_string(),
        format!("  Configured       {}", servers.len()),
    ];

    if servers.is_empty() {
        lines.push("  No MCP servers are configured.".to_string());
        lines.push("  Add a server to project/user Claw settings, then rerun this command.".to_string());
        return lines.join("\n");
    }

    lines.push(String::new());
    lines.push("Configured servers".to_string());
    for (name, server) in servers {
        let scope = format_config_scope(server.scope);
        let transport = transport_name(server.transport());
        lines.push(format!("  {name:<24} {transport:<14} scope={scope}"));
        append_server_target(&mut lines, server);
    }

    lines.join("\n")
}

fn format_config_scope(scope: ConfigSource) -> &'static str {
    match scope {
        ConfigSource::User => "user",
        ConfigSource::Project => "project",
        ConfigSource::Local => "local",
    }
}

fn transport_name(transport: McpTransport) -> &'static str {
    match transport {
        McpTransport::Stdio => "stdio",
        McpTransport::Sse => "sse",
        McpTransport::Http => "http",
        McpTransport::Ws => "websocket",
        McpTransport::Sdk => "sdk",
        McpTransport::ManagedProxy => "managed-proxy",
    }
}

fn append_server_target(lines: &mut Vec<String>, server: &ScopedMcpServerConfig) {
    match &server.config {
        McpServerConfig::Stdio(config) => {
            let mut command = config.command.clone();
            for arg in &config.args {
                let _ = write!(command, " {arg}");
            }
            lines.push(format!("    target           {command}"));
        }
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => {
            lines.push(format!("    target           {}", config.url));
        }
        McpServerConfig::Ws(config) => {
            lines.push(format!("    target           {}", config.url));
        }
        McpServerConfig::Sdk(config) => {
            lines.push(format!("    target           {}", config.name));
        }
        McpServerConfig::ManagedProxy(config) => {
            lines.push(format!("    target           {} ({})", config.url, config.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_config_scope, render_mcp_report, transport_name};
    use runtime::{ConfigSource, McpTransport, ScopedMcpServerConfig};
    use std::collections::BTreeMap;

    #[test]
    fn renders_empty_mcp_report() {
        let servers = BTreeMap::new();
        let report = render_mcp_report(&servers);
        assert!(report.contains("Configured       0"));
        assert!(report.contains("No MCP servers are configured."));
    }

    #[test]
    fn maps_scopes_and_transports_to_stable_labels() {
        assert_eq!(format_config_scope(ConfigSource::User), "user");
        assert_eq!(format_config_scope(ConfigSource::Project), "project");
        assert_eq!(format_config_scope(ConfigSource::Local), "local");
        assert_eq!(transport_name(McpTransport::Stdio), "stdio");
        assert_eq!(transport_name(McpTransport::Sse), "sse");
        assert_eq!(transport_name(McpTransport::Http), "http");
        assert_eq!(transport_name(McpTransport::Ws), "websocket");
        assert_eq!(transport_name(McpTransport::Sdk), "sdk");
        assert_eq!(transport_name(McpTransport::ManagedProxy), "managed-proxy");
    }

    #[allow(clippy::missing_const_for_fn)]
    fn _typecheck_server_shape(server: ScopedMcpServerConfig) {
        let mut servers = BTreeMap::new();
        servers.insert("example".to_string(), server);
        let _ = render_mcp_report(&servers);
    }
}
