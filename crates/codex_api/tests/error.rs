use reqwest::StatusCode;

use codex_api::error::parse_error_message;

#[test]
fn parse_error_message_is_friendly_on_usage_limit() {
    let body = r#"{"error":{"code":"usage_limit_reached","message":"cap exceeded","plan_type":"Pro","resets_at":1731234567}}"#;

    let message = parse_error_message(StatusCode::TOO_MANY_REQUESTS, body);
    assert!(message.contains("You have hit your ChatGPT usage limit"));
    assert!(message.contains("pro plan"));
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

#[test]
fn parse_error_message_empty_non_json_body_falls_back_to_status_reason() {
    let message = parse_error_message(StatusCode::BAD_GATEWAY, "");
    assert_eq!(message, "Bad Gateway");
}

#[test]
fn parse_error_message_whitespace_non_json_body_preserves_raw_text() {
    let message = parse_error_message(StatusCode::SERVICE_UNAVAILABLE, "   \n\t");
    assert_eq!(message, "   \n\t");
}

#[test]
fn parse_error_message_whitespace_json_message_preserves_raw_text() {
    let body = r#"{"error":{"message":"   "}}"#;
    let message = parse_error_message(StatusCode::BAD_REQUEST, body);
    assert_eq!(message, "   ");
}

#[test]
fn parse_error_message_uses_error_type_when_code_is_empty() {
    let body = r#"{"error":{"code":"","type":"usage_limit_reached","message":"cap exceeded","plan_type":"Pro"}}"#;
    let message = parse_error_message(StatusCode::BAD_REQUEST, body);
    assert!(message.contains("You have hit your ChatGPT usage limit"));
    assert!(message.contains("pro plan"));
}

#[test]
fn parse_error_message_usage_limit_ignores_empty_plan_and_zero_reset_hint() {
    let body = r#"{"error":{"code":"usage_limit_reached","message":"cap exceeded","plan_type":"","resets_at":0}}"#;
    let message = parse_error_message(StatusCode::TOO_MANY_REQUESTS, body);
    assert!(message.contains("You have hit your ChatGPT usage limit"));
    assert!(!message.contains("( plan)"));
    assert!(!message.contains("Try again in ~"));
}
