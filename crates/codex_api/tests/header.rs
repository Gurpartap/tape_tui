use base64::{engine::general_purpose, Engine as _};
use codex_api::headers::{
    build_headers, HEADER_ACCEPT, HEADER_ACCOUNT_ID, HEADER_CONTENT_TYPE, HEADER_OPENAI_BETA,
    HEADER_ORIGINATOR, HEADER_SESSION_ID, HEADER_USER_AGENT,
};
use codex_api::{CodexApiConfig, CodexApiError};
use serde_json::json;

#[test]
fn header_map_contains_codex_headers() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "account-id"}
    }));
    let expected_auth = format!("Bearer {token}");
    let config = CodexApiConfig::new(token)
        .with_session_id("session-42")
        .with_originator("pi")
        .insert_header("x-extra", "value");

    let headers = build_headers(&config, None).expect("header construction");
    assert_eq!(
        headers.get("authorization").expect("authorization header"),
        &expected_auth
    );
    assert_eq!(
        headers
            .get(HEADER_ACCOUNT_ID)
            .expect("legacy account-id header"),
        &"account-id".to_owned()
    );
    assert_eq!(
        headers.get(HEADER_OPENAI_BETA).expect("openai beta"),
        &"responses=experimental".to_owned()
    );
    assert_eq!(
        headers.get(HEADER_ORIGINATOR).expect("originator"),
        &"pi".to_owned()
    );
    assert_eq!(
        headers.get(HEADER_ACCEPT).expect("accept"),
        &"text/event-stream".to_owned()
    );
    assert_eq!(
        headers.get(HEADER_CONTENT_TYPE).expect("content-type"),
        &"application/json".to_owned()
    );
    assert_eq!(
        headers.get(HEADER_SESSION_ID).expect("session-id"),
        &"session-42".to_owned()
    );
    assert_eq!(headers.get("x-extra").expect("custom"), &"value".to_owned());
    let user_agent = headers.get(HEADER_USER_AGENT).expect("default user-agent");
    assert!(
        user_agent == "pi (browser)" || user_agent.starts_with("pi ("),
        "default user-agent should mirror PI shape"
    );
}

#[test]
fn header_map_prefers_deterministic_user_agent() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "account-id"}
    }));
    let config = CodexApiConfig::new(token);
    let headers = build_headers(&config, Some("test-agent")).expect("header construction");
    assert_eq!(
        headers.get(HEADER_USER_AGENT).expect("user-agent"),
        &"test-agent".to_string()
    );
}

#[test]
fn header_map_extracts_account_id_from_token_namespace_claim() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "acct-ns"}
    }));
    let config = CodexApiConfig::new(token);
    let headers = build_headers(&config, None).expect("header construction");

    assert_eq!(
        headers.get(HEADER_ACCOUNT_ID).expect("account-id header"),
        &"acct-ns".to_owned()
    );
}

#[test]
fn header_map_rejects_legacy_claim_without_namespace() {
    let token = token_with_claims(json!({"chatgpt_account_id": "acct-legacy"}));
    let config = CodexApiConfig::new(token);
    let error = build_headers(&config, None).expect_err("account-id should be required");
    assert!(matches!(error, CodexApiError::MissingAccountId));
}

#[test]
fn header_map_rejects_organizations_claim_without_namespace() {
    let token = token_with_claims(json!({"organizations": [{"id": "org-1"}]}));
    let config = CodexApiConfig::new(token);
    let error = build_headers(&config, None).expect_err("account-id should be required");
    assert!(matches!(error, CodexApiError::MissingAccountId));
}

#[test]
fn header_map_uses_token_claim_over_explicit_account_id() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "acct-token"}
    }));
    let config = CodexApiConfig::new(token).with_account_id("acct-explicit");
    let headers = build_headers(&config, None).expect("header construction");

    assert_eq!(
        headers.get(HEADER_ACCOUNT_ID).expect("account-id header"),
        &"acct-token".to_owned()
    );
}

#[test]
fn header_map_rejects_missing_account_id_when_not_in_token() {
    let token = token_with_claims(json!({"sub": "user"}));
    let config = CodexApiConfig::new(token);
    let error = build_headers(&config, None).expect_err("account-id should be required");
    assert!(matches!(error, CodexApiError::MissingAccountId));
}

#[test]
fn header_map_session_id_overrides_extra_headers() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "account-id"}
    }));
    let config = CodexApiConfig::new(token)
        .insert_header(HEADER_SESSION_ID, "session-from-extra")
        .with_session_id("session-from-config");
    let headers = build_headers(&config, None).expect("header construction");

    assert_eq!(
        headers.get(HEADER_SESSION_ID).expect("session-id"),
        &"session-from-config".to_owned()
    );
}

#[test]
fn header_map_rejects_non_jwt_tokens() {
    let config = CodexApiConfig::new("segment.without-jwt-shape");
    let error = build_headers(&config, None).expect_err("account-id should be required");
    assert!(matches!(error, CodexApiError::MissingAccountId));
}

#[test]
fn header_map_extra_headers_override_required_headers_case_insensitively() {
    let token = token_with_claims(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "account-id"}
    }));
    let config = CodexApiConfig::new(token).insert_header("Authorization", "Bearer override");
    let headers = build_headers(&config, None).expect("header construction");

    assert_eq!(
        headers.get("authorization").expect("authorization header"),
        &"Bearer override".to_owned()
    );
}

fn token_with_claims(claims: serde_json::Value) -> String {
    let payload = serde_json::to_vec(&claims).expect("serialize token claims");
    let payload = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    format!("header.{payload}.signature")
}
