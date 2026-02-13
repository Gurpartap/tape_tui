use codex_api::normalize_codex_url;

#[test]
fn url_normalization_keeps_existing_responses_endpoint() {
    assert_eq!(
        normalize_codex_url("https://chatgpt.com/backend-api/codex/responses"),
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[test]
fn url_normalization_appends_responses_to_codex_base() {
    assert_eq!(
        normalize_codex_url("https://chatgpt.com/backend-api/codex"),
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[test]
fn url_normalization_appends_codex_responses_to_generic_base() {
    assert_eq!(
        normalize_codex_url("https://chatgpt.com/backend-api"),
        "https://chatgpt.com/backend-api/codex/responses"
    );
}
