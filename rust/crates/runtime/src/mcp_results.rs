use serde_json::Value;

use crate::mcp_stdio::McpToolCallResult;

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolOutput {
    pub text: String,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl McpToolOutput {
    #[must_use]
    pub fn from_result(result: McpToolCallResult) -> Self {
        let text = result
            .content
            .into_iter()
            .filter(|content| content.kind == "text")
            .filter_map(|content| content.data.get("text").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n");

        let structured_content = result.structured_content;
        let is_error = result.is_error.unwrap_or(false);
        Self {
            text,
            structured_content,
            is_error,
        }
    }

    #[must_use]
    pub fn render_for_model(&self) -> String {
        match (&self.text.is_empty(), &self.structured_content) {
            (false, Some(structured)) => format!("{}\n\nStructured output:\n{}", self.text, structured),
            (false, None) => self.text.clone(),
            (true, Some(structured)) => structured.to_string(),
            (true, None) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::mcp_stdio::{McpToolCallContent, McpToolCallResult};

    use super::McpToolOutput;

    #[test]
    fn preserves_text_and_structured_mcp_output() {
        let output = McpToolOutput::from_result(McpToolCallResult {
            content: vec![
                McpToolCallContent {
                    kind: "text".to_string(),
                    data: std::collections::BTreeMap::from([("text".to_string(), json!("hello"))]),
                },
                McpToolCallContent {
                    kind: "image".to_string(),
                    data: std::collections::BTreeMap::new(),
                },
            ],
            structured_content: Some(json!({"answer": 42})),
            is_error: Some(false),
            meta: None,
        });

        assert_eq!(output.text, "hello");
        assert_eq!(output.structured_content, Some(json!({"answer": 42})));
        assert!(!output.is_error);
        assert_eq!(output.render_for_model(), "hello\n\nStructured output:\n{\"answer\":42}");
    }

    #[test]
    fn falls_back_to_structured_output_when_text_is_absent() {
        let output = McpToolOutput::from_result(McpToolCallResult {
            content: Vec::new(),
            structured_content: Some(json!({"ok": true})),
            is_error: Some(false),
            meta: None,
        });

        assert_eq!(output.render_for_model(), "{\"ok\":true}");
    }

    #[test]
    fn defaults_missing_error_flag_to_success() {
        let output = McpToolOutput::from_result(McpToolCallResult {
            content: Vec::new(),
            structured_content: None,
            is_error: None,
            meta: None,
        });

        assert!(!output.is_error);
        assert!(output.render_for_model().is_empty());
    }
}
