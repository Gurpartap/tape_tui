use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical request payload shape for Codex responses endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub input: Value,
    /// Default: false.
    #[serde(default)]
    pub store: bool,
    /// Default: true.
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default)]
    pub text: CodexRequestText,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(rename = "tool_choice", skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(rename = "parallel_tool_calls", default)]
    pub parallel_tool_calls: bool,
    #[serde(rename = "prompt_cache_key", skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<CodexReasoning>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
}

fn default_true() -> bool {
    true
}

impl CodexRequest {
    pub fn new(
        model: impl Into<String>,
        input: impl Into<Value>,
        instructions: Option<String>,
    ) -> Self {
        Self {
            model: model.into(),
            input: input.into(),
            instructions,
            store: false,
            stream: true,
            text: CodexRequestText::default(),
            include: vec!["reasoning.encrypted_content".to_string()],
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: true,
            prompt_cache_key: None,
            temperature: None,
            reasoning: None,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexRequestText {
    pub verbosity: String,
}

impl Default for CodexRequestText {
    fn default() -> Self {
        Self {
            verbosity: "medium".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexReasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
