use serde_json::json;

use codex_api::{normalize_codex_url, CodexApiClient, CodexApiConfig, CodexRequest};

#[test]
fn smoke_client_constructs_from_config() {
    let config = CodexApiConfig::new("token")
        .with_account_id("account")
        .with_session_id("session-1")
        .with_originator("pi");

    let client = CodexApiClient::new(config.clone()).expect("client creation should succeed");
    assert_eq!(
        normalize_codex_url("https://chatgpt.com/backend-api"),
        client.normalized_endpoint()
    );
    assert_eq!("token", client.config().access_token);
    assert_eq!("account", client.config().account_id);
    assert_eq!(Some("session-1".to_string()), client.config().session_id);
}

#[test]
fn default_request_has_parity_defaults() {
    let request = CodexRequest::new(
        "gpt-codex",
        json!([{"role":"user"}]),
        Some("sys".to_string()),
    );
    assert!(!request.store);
    assert!(request.stream);
    assert_eq!(request.text.verbosity, "medium");
    assert_eq!(request.tool_choice.as_deref(), Some("auto"));
    assert!(request.parallel_tool_calls);
    assert_eq!(request.include, vec!["reasoning.encrypted_content"]);
}
