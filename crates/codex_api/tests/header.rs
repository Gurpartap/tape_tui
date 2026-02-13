use codex_api::headers::{
    build_headers, HEADER_ACCEPT, HEADER_ACCOUNT_ID, HEADER_ACCOUNT_ID_CANONICAL,
    HEADER_CONTENT_TYPE, HEADER_OPENAI_BETA, HEADER_ORIGINATOR, HEADER_SESSION_ID,
    HEADER_USER_AGENT,
};
use codex_api::CodexApiConfig;

#[test]
fn header_map_contains_codex_headers() {
    let config = CodexApiConfig::new("access-token", "account-id")
        .with_session_id("session-42")
        .with_originator("pi")
        .insert_header("x-extra", "value");

    let headers = build_headers(&config, None).expect("header construction");
    assert_eq!(
        headers.get("authorization").expect("authorization header"),
        &"Bearer access-token".to_owned()
    );
    assert_eq!(
        headers
            .get(HEADER_ACCOUNT_ID)
            .expect("legacy account-id header"),
        &"account-id".to_owned()
    );
    assert_eq!(
        headers
            .get(HEADER_ACCOUNT_ID_CANONICAL)
            .expect("canonical account-id header"),
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
}

#[test]
fn header_map_prefers_deterministic_user_agent() {
    let config = CodexApiConfig::new("access-token", "account-id");
    let headers = build_headers(&config, Some("test-agent")).expect("header construction");
    assert_eq!(
        headers.get(HEADER_USER_AGENT).expect("user-agent"),
        &"test-agent".to_string()
    );
}
