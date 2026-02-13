use base64::{engine::general_purpose, Engine as _};
use codex_api::payload::CodexReasoning;
use codex_api::{CodexApiClient, CodexApiConfig, CodexRequest};
use serde_json::{json, Value};

#[test]
fn payload_serialization_defaults_match_parity_shape() {
    let request = CodexRequest::new("gpt-codex", user_input("hi"), Some("sys".to_string()));
    let body = serde_json::to_value(&request).expect("serialize payload");

    assert_eq!(body["store"], Value::Bool(false));
    assert_eq!(body["stream"], Value::Bool(true));
    assert_eq!(
        body["text"]["verbosity"],
        Value::String("medium".to_string())
    );
    assert_eq!(
        body["include"],
        Value::Array(vec![Value::String(
            "reasoning.encrypted_content".to_string()
        )])
    );
    assert_eq!(body["tool_choice"], Value::String("auto".to_string()));
    assert_eq!(body["parallel_tool_calls"], Value::Bool(true));
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body.get("reasoning").is_none());
    assert!(body.get("tools").is_none());
}

#[test]
fn payload_serialization_includes_optional_fields_when_set() {
    let mut request = CodexRequest::new("gpt-codex", user_input("hi"), Some("sys".to_string()));
    request.prompt_cache_key = Some("session-1".to_string());
    request.temperature = Some(0.2);
    request.reasoning = Some(CodexReasoning {
        effort: Some("low".to_string()),
        summary: Some("auto".to_string()),
    });
    request.tools = vec![json!({
        "type": "function",
        "name": "test_tool",
    })];

    let body = serde_json::to_value(&request).expect("serialize payload");
    assert_eq!(
        body["prompt_cache_key"],
        Value::String("session-1".to_string())
    );
    assert_eq!(body["temperature"], json!(0.2));
    assert_eq!(
        body["reasoning"]["effort"],
        Value::String("low".to_string())
    );
    assert_eq!(
        body["reasoning"]["summary"],
        Value::String("auto".to_string())
    );
    assert_eq!(
        body["tools"][0]["name"],
        Value::String("test_tool".to_string())
    );
}

#[test]
fn build_request_uses_session_id_for_prompt_cache_key_when_missing() {
    let request = CodexRequest::new("gpt-codex", user_input("payload"), None);
    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api")
        .with_session_id("session-42");
    let client = CodexApiClient::new(config).expect("client");

    let http_request = client
        .build_request(&request)
        .expect("build request")
        .build()
        .expect("request");
    let body = request_body_json(&http_request);

    assert_eq!(
        body["prompt_cache_key"],
        Value::String("session-42".to_string())
    );
}

#[test]
fn build_request_preserves_explicit_prompt_cache_key() {
    let mut request = CodexRequest::new("gpt-codex", user_input("payload"), None);
    request.prompt_cache_key = Some("explicit-key".to_string());

    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api")
        .with_session_id("session-42");
    let client = CodexApiClient::new(config).expect("client");

    let http_request = client
        .build_request(&request)
        .expect("build request")
        .build()
        .expect("request");
    let body = request_body_json(&http_request);

    assert_eq!(
        body["prompt_cache_key"],
        Value::String("explicit-key".to_string())
    );
}

#[test]
fn build_request_enforces_pi_transport_defaults() {
    let mut request = CodexRequest::new("gpt-codex", user_input("payload"), None);
    request.store = true;
    request.stream = false;
    request.text.verbosity = String::new();
    request.include = vec!["custom.include".to_owned()];
    request.tool_choice = Some("required".to_owned());
    request.parallel_tool_calls = false;

    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");

    let http_request = client
        .build_request(&request)
        .expect("build request")
        .build()
        .expect("request");
    let body = request_body_json(&http_request);

    assert_eq!(body["store"], Value::Bool(false));
    assert_eq!(body["stream"], Value::Bool(true));
    assert_eq!(
        body["text"]["verbosity"],
        Value::String("medium".to_owned())
    );
    assert_eq!(
        body["include"],
        Value::Array(vec![Value::String(
            "reasoning.encrypted_content".to_owned()
        )])
    );
    assert_eq!(body["tool_choice"], Value::String("auto".to_owned()));
    assert_eq!(body["parallel_tool_calls"], Value::Bool(true));
}

#[test]
fn build_request_clamps_reasoning_effort_and_sets_summary_default() {
    let mut request = CodexRequest::new("gpt-5.3-codex", user_input("payload"), None);
    request.reasoning = Some(CodexReasoning {
        effort: Some("minimal".to_owned()),
        summary: None,
    });

    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");

    let http_request = client
        .build_request(&request)
        .expect("build request")
        .build()
        .expect("request");
    let body = request_body_json(&http_request);

    assert_eq!(body["reasoning"]["effort"], Value::String("low".to_owned()));
    assert_eq!(
        body["reasoning"]["summary"],
        Value::String("auto".to_owned())
    );
}

#[test]
fn build_request_clamps_model_specific_reasoning_effort_variants() {
    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");

    let mut gpt_51 = CodexRequest::new("gpt-5.1", user_input("payload"), None);
    gpt_51.reasoning = Some(CodexReasoning {
        effort: Some("xhigh".to_owned()),
        summary: Some("concise".to_owned()),
    });
    let gpt_51_request = client
        .build_request(&gpt_51)
        .expect("build request")
        .build()
        .expect("request");
    let gpt_51_body = request_body_json(&gpt_51_request);
    assert_eq!(
        gpt_51_body["reasoning"]["effort"],
        Value::String("high".to_owned())
    );
    assert_eq!(
        gpt_51_body["reasoning"]["summary"],
        Value::String("concise".to_owned())
    );

    let mut codex_mini = CodexRequest::new("gpt-5.1-codex-mini", user_input("payload"), None);
    codex_mini.reasoning = Some(CodexReasoning {
        effort: Some("low".to_owned()),
        summary: None,
    });
    let codex_mini_request = client
        .build_request(&codex_mini)
        .expect("build request")
        .build()
        .expect("request");
    let codex_mini_body = request_body_json(&codex_mini_request);
    assert_eq!(
        codex_mini_body["reasoning"]["effort"],
        Value::String("medium".to_owned())
    );
    assert_eq!(
        codex_mini_body["reasoning"]["summary"],
        Value::String("auto".to_owned())
    );
}

#[test]
fn build_request_rejects_non_list_input_preflight() {
    let request = CodexRequest::new("gpt-codex", json!("payload"), None);
    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");

    let error = client
        .build_request(&request)
        .expect_err("string input should fail request preflight");

    assert!(matches!(
        error,
        codex_api::CodexApiError::InvalidRequestPayload(ref message)
            if message == "'input' must be a JSON array/list, got string"
    ));
}

fn user_input(text: &str) -> Value {
    json!([
        {
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": text,
                }
            ],
        }
    ])
}

fn request_body_json(request: &reqwest::Request) -> Value {
    let body = request
        .body()
        .expect("request should carry JSON body")
        .as_bytes()
        .expect("JSON body should be buffered bytes");
    serde_json::from_slice::<Value>(body).expect("request body should be valid JSON")
}

fn token_with_account_id(account_id: &str) -> String {
    let claims = json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
    });
    let payload = serde_json::to_vec(&claims).expect("serialize token claims");
    let payload = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    format!("header.{payload}.signature")
}
