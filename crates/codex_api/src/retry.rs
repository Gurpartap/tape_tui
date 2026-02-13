use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

/// Maximum retry attempts after an initial request attempt.
pub const MAX_RETRIES: u32 = 3;
/// Base delay before the first retry.
pub const BASE_DELAY_MS: u64 = 1000;

fn retryable_status_regex() -> &'static Regex {
    static CACHED: OnceLock<Regex> = OnceLock::new();
    CACHED.get_or_init(|| {
        Regex::new(r"(?i)rate.?limit|overloaded|service.?unavailable|upstream.?connect|connection.?refused")
            .expect("retry regex must compile")
    })
}

/// Error text retry policy for transient failures and retryable statuses.
pub fn is_retryable_http_error(status: u16, error_text: &str) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504) || retryable_status_regex().is_match(error_text)
}

/// Compute exponential backoff delay for a retry attempt.
pub fn retry_delay_ms(attempt: u32) -> Duration {
    let exponent = attempt.min(30);
    Duration::from_millis(BASE_DELAY_MS * 2u64.saturating_pow(exponent))
}
