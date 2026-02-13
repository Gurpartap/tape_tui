/// Default base URL for Codex transport requests.
pub const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Normalize a base URL to a Codex responses endpoint.
///
/// Normalization rules:
/// 1) keep `/codex/responses` unchanged
/// 2) append `/responses` when path ends in `/codex`
/// 3) append `/codex/responses` otherwise
pub fn normalize_codex_url(input: &str) -> String {
    let base = if input.trim().is_empty() {
        DEFAULT_CODEX_BASE_URL
    } else {
        input.trim()
    };

    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/codex/responses") {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/codex") {
        return format!("{trimmed}/responses");
    }
    format!("{trimmed}/codex/responses")
}
