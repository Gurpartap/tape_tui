use base64::{engine::general_purpose, Engine as _};
use codex_api::events::CodexStreamEvent;
use codex_api::{normalize_codex_url, CodexApiClient, CodexApiConfig, CodexRequest};
use serde_json::json;

#[test]
fn http_request_builds_codex_endpoint() {
    let config = CodexApiConfig::new(token_with_account_id("account"))
        .with_base_url("https://chatgpt.com/backend-api");
    let client = CodexApiClient::new(config).expect("client");
    let request = CodexRequest::new(
        "model",
        serde_json::json!([
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "payload",
                    }
                ],
            }
        ]),
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
    let completed = CodexStreamEvent::ResponseCompleted {
        status: Some(codex_api::events::CodexResponseStatus::Completed),
    };
    let completed_json = serde_json::to_value(&completed).expect("serialize completed event");
    assert_eq!(completed_json["type"], "response.completed");
    assert_eq!(completed_json["status"], "completed");

    let delta = CodexStreamEvent::OutputTextDelta {
        delta: "hello".to_string(),
    };
    let delta_json = serde_json::to_value(&delta).expect("serialize output text delta event");
    assert_eq!(delta_json["type"], "response.output_text.delta");
    assert_eq!(delta_json["delta"], "hello");
}

fn token_with_account_id(account_id: &str) -> String {
    let claims = json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
    });
    let payload = serde_json::to_vec(&claims).expect("serialize token claims");
    let payload = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    format!("header.{payload}.signature")
}
