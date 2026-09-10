from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{path}: expected exactly one match, got {count}: {old[:120]!r}")
    file.write_text(text.replace(old, new))


# commands crate: make /mcp a shared, inspect-only slash command and expose the renderer.
replace_once(
    "rust/crates/commands/src/lib.rs",
    "pub use hooks::handle_hooks_slash_command;\n",
    "pub use hooks::handle_hooks_slash_command;\n\nuse std::fmt::Write as _;\nuse runtime::{ConfigSource, McpServerConfig, McpTransport, ScopedMcpServerConfig};\n",
)
replace_once(
    "rust/crates/commands/src/lib.rs",
    '    SlashCommandSpec {\n        name: "hooks",',
    '    SlashCommandSpec {\n        name: "mcp",\n        aliases: &[],\n        summary: "Inspect configured MCP servers",\n        argument_hint: None,\n        resume_supported: true,\n        category: SlashCommandCategory::Automation,\n    },\n    SlashCommandSpec {\n        name: "hooks",',
)
replace_once(
    "rust/crates/commands/src/lib.rs",
    "pub enum SlashCommand {\n    Help,\n    Status,\n    Compact,",
    "pub enum SlashCommand {\n    Help,\n    Status,\n    Mcp,\n    Compact,",
)
replace_once(
    "rust/crates/commands/src/lib.rs",
    '            "help" => Self::Help,\n            "status" => Self::Status,\n            "hooks" => Self::Config {',
    '            "help" => Self::Help,\n            "status" => Self::Status,\n            "mcp" => Self::Mcp,\n            "hooks" => Self::Config {',
)

marker = '\nfn remainder_after_command(input: &str, command: &str) -> Option<String> {'
commands = Path("rust/crates/commands/src/lib.rs")
text = commands.read_text()
if "pub fn render_mcp_report(" not in text:
    renderer = r'''

#[must_use]
pub fn render_mcp_report(
    servers: &std::collections::BTreeMap<String, ScopedMcpServerConfig>,
) -> String {
    let mut lines = vec![
        "MCP servers".to_string(),
        format!("  Configured       {}", servers.len()),
    ];

    if servers.is_empty() {
        lines.push("  No MCP servers are configured.".to_string());
        lines.push(
            "  Add a server to project/user Claw settings, then rerun this command.".to_string(),
        );
        return lines.join("\n");
    }

    lines.push(String::new());
    lines.push("Configured servers".to_string());
    for (name, server) in servers {
        let scope = match server.scope {
            ConfigSource::User => "user",
            ConfigSource::Project => "project",
            ConfigSource::Local => "local",
        };
        let transport = match server.transport() {
            McpTransport::Stdio => "stdio",
            McpTransport::Sse => "sse",
            McpTransport::Http => "http",
            McpTransport::Ws => "websocket",
            McpTransport::Sdk => "sdk",
            McpTransport::ManagedProxy => "managed-proxy",
        };
        lines.push(format!("  {name:<24} {transport:<14} scope={scope}"));
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

    lines.join("\n")
}
'''
    if marker not in text:
        raise RuntimeError("commands lib: insertion marker not found")
    commands.write_text(text.replace(marker, renderer + marker))

# claw-cli: dispatch the shared command and expose the report in the live REPL.
replace_once(
    "rust/crates/claw-cli/src/main.rs",
    "    handle_skills_slash_command, render_slash_command_help, resume_supported_slash_commands,\n",
    "    handle_skills_slash_command, render_mcp_report, render_slash_command_help,\n    resume_supported_slash_commands,\n",
)
replace_once(
    "rust/crates/claw-cli/src/main.rs",
    "            SlashCommand::Status => {\n                self.print_status();\n                false\n            }\n            SlashCommand::Bughunter",
    "            SlashCommand::Status => {\n                self.print_status();\n                false\n            }\n            SlashCommand::Mcp => {\n                Self::print_mcp()?;\n                false\n            }\n            SlashCommand::Bughunter",
)
replace_once(
    "rust/crates/claw-cli/src/main.rs",
    "    fn print_config(section: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {",
    "    fn print_mcp() -> Result<(), Box<dyn std::error::Error>> {\n        let cwd = env::current_dir()?;\n        let config = ConfigLoader::default_for(&cwd).load()?;\n        println!(\"{}\", render_mcp_report(config.mcp().servers()));\n        Ok(())\n    }\n\n    fn print_config(section: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {",
)
replace_once(
    "rust/crates/claw-cli/src/main.rs",
    "        SlashCommand::Config { section } => Ok(ResumeCommandOutcome {\n            session: session.clone(),\n            message: Some(render_config_report(section.as_deref())?),\n        }),",
    "        SlashCommand::Mcp => Ok(ResumeCommandOutcome {\n            session: session.clone(),\n            message: Some({\n                let cwd = env::current_dir()?;\n                let config = ConfigLoader::default_for(&cwd).load()?;\n                render_mcp_report(config.mcp().servers())\n            }),\n        }),\n        SlashCommand::Config { section } => Ok(ResumeCommandOutcome {\n            session: session.clone(),\n            message: Some(render_config_report(section.as_deref())?),\n        }),",
)
replace_once(
    "rust/crates/claw-cli/src/main.rs",
    '    #[test]\n    fn parses_direct_agents_and_skills_slash_commands() {',
    '    #[test]\n    fn parses_mcp_as_interactive_command() {\n        assert_eq!(\n            super::SlashCommand::parse("/mcp"),\n            Some(super::SlashCommand::Mcp)\n        );\n    }\n\n    #[test]\n    fn parses_direct_agents_and_skills_slash_commands() {',
)

# Keep the standalone binary as a thin wrapper over the shared report implementation.
Path("rust/crates/commands/src/bin/mcp.rs").write_text(
    '''use std::env;\n\nuse commands::render_mcp_report;\nuse runtime::ConfigLoader;\n\nfn main() -> Result<(), Box<dyn std::error::Error>> {\n    let cwd = env::current_dir()?;\n    let config = ConfigLoader::default_for(&cwd).load()?;\n    println!("{}", render_mcp_report(config.mcp().servers()));\n    Ok(())\n}\n\n#[cfg(test)]\nmod tests {\n    use commands::render_mcp_report;\n    use std::collections::BTreeMap;\n\n    #[test]\n    fn renders_empty_mcp_report() {\n        let report = render_mcp_report(&BTreeMap::new());\n        assert!(report.contains("Configured       0"));\n        assert!(report.contains("No MCP servers are configured."));\n    }\n}\n'''
)

# Mark the implementation in the roadmap.
parity = Path("PARITY.md")
text = parity.read_text()
text = text.replace(
    "- First-class interactive `/mcp`, `/plan`, `/review`, `/tasks`, and related command-family parity",
    "- First-class interactive `/plan`, `/review`, `/tasks`, and related command-family parity",
)
text = text.replace(
    "- **Phase 9B:** added local `/hooks add` and `/hooks remove` persistence while preserving merged runtime defaults.\n",
    "- **Phase 9B:** added local `/hooks add` and `/hooks remove` persistence while preserving merged runtime defaults.\n- **Phase 9D:** wired the runtime-backed MCP inspector into the interactive `/mcp` slash-command registry.\n",
)
text = text.replace(
    "1. Finish the MCP command family by wiring the inspector into the interactive `/mcp` slash-command registry.",
    "1. Expand `/mcp` beyond inspection into connection lifecycle and richer interactive MCP management.",
)
parity.write_text(text)
