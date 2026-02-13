use codex_api::retry::*;

#[test]
fn retry_http_status_is_retryable() {
    assert!(is_retryable_http_error(429, ""));
    assert!(is_retryable_http_error(500, ""));
    assert!(is_retryable_http_error(502, ""));
    assert!(is_retryable_http_error(503, ""));
    assert!(is_retryable_http_error(504, ""));
}

#[test]
fn retry_http_error_pattern_is_retryable() {
    assert!(is_retryable_http_error(400, "rate limit exceeded"));
    assert!(is_retryable_http_error(400, "connection refused"));
}

#[test]
fn retry_delay_is_exponential() {
    assert_eq!(retry_delay_ms(0).as_millis(), 1000);
    assert_eq!(retry_delay_ms(1).as_millis(), 2000);
    assert_eq!(retry_delay_ms(2).as_millis(), 4000);
}
