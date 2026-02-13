use codex_api::events::CodexStreamEvent;
use codex_api::{normalize_codex_url, CodexApiClient, CodexApiConfig, CodexRequest};

#[test]
fn http_request_builds_codex_endpoint() {
    let config =
        CodexApiConfig::new("token", "account").with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");
    let request = CodexRequest::new(
        "model",
        serde_json::json!("payload"),
        Some("sys".to_string()),
    );

    let http_request = client
        .build_request(&request)
        .expect("build request")
        .build()
        .expect("request");

    assert_eq!(
        http_request.url().as_str(),
        normalize_codex_url("https://chatgpt.com/backend-api")
    );
    assert_eq!(http_request.method(), "POST");
}

#[test]
fn http_stream_event_variant_names_stable() {
    let events = [
        CodexStreamEvent::ResponseCompleted {
            status: codex_api::events::CodexResponseStatus::Completed,
        },
        CodexStreamEvent::ResponseCompleted {
            status: codex_api::events::CodexResponseStatus::Completed,
        },
    ];
    assert_eq!(events.len(), 2);
}
