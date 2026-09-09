// Phase 8D: machine-readable output must not render terminal UI before the payload.

use std::fs;
use std::path::PathBuf;

#[test]
fn cli_app_keeps_terminal_ui_out_of_machine_output_modes() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("app.rs");
    let source = fs::read_to_string(path).expect("read cli app source");

    assert!(source.contains("let machine_output = self.config.output_format != OutputFormat::Text;"));
    assert!(source.contains("if !machine_output {\n            stream_spinner.tick("));
    assert!(source.contains("if !machine_output {\n                        Self::handle_stream_event("));
    assert!(source.contains("if !machine_output {\n                    stream_spinner.fail("));
    assert!(source.contains("if !machine_output {\n            if saw_text {"));
}
