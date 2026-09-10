use std::env;

use commands::render_mcp_report;
use runtime::ConfigLoader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    println!("{}", render_mcp_report(config.mcp().servers()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use commands::render_mcp_report;
    use std::collections::BTreeMap;

    #[test]
    fn renders_empty_mcp_report() {
        let report = render_mcp_report(&BTreeMap::new());
        assert!(report.contains("Configured       0"));
        assert!(report.contains("No MCP servers are configured."));
    }
}
