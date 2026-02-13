use reqwest::StatusCode;

use codex_api::error::parse_error_message;

#[test]
fn parse_error_message_is_friendly_on_usage_limit() {
    let body = r#"{"error":{"code":"usage_limit_reached","message":"cap exceeded","plan_type":"Pro","resets_at":1731234567}}"#;

    let message = parse_error_message(StatusCode::TOO_MANY_REQUESTS, body);
    assert!(message.contains("You have hit your ChatGPT usage limit"));
    assert!(message.contains("Pro plan"));
}

#[test]
fn parse_error_message_is_message_fallback_when_json_has_message() {
    let body = r#"{"error":{"code":"bad_request","message":"invalid model"}}"#;
    let message = parse_error_message(StatusCode::BAD_REQUEST, body);
    assert_eq!(message, "invalid model");
}

#[test]
fn parse_error_message_falls_back_to_raw_body() {
    let body = "raw failure text";
    let message = parse_error_message(StatusCode::INTERNAL_SERVER_ERROR, body);
    assert_eq!(message, "raw failure text");
}
