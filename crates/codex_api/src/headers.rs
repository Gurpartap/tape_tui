use std::collections::BTreeMap;

use crate::config::CodexApiConfig;
use crate::error::CodexApiError;

pub const HEADER_SESSION_ID: &str = "session_id";
pub const HEADER_ACCEPT: &str = "accept";
pub const HEADER_CONTENT_TYPE: &str = "content-type";
pub const HEADER_AUTHORIZATION: &str = "authorization";
pub const HEADER_ACCOUNT_ID: &str = "chatgpt-account-id";
pub const HEADER_ACCOUNT_ID_CANONICAL: &str = "ChatGPT-Account-Id";
pub const HEADER_OPENAI_BETA: &str = "OpenAI-Beta";
pub const HEADER_ORIGINATOR: &str = "originator";
pub const HEADER_USER_AGENT: &str = "User-Agent";

/// Build a deterministic header map for Codex transport requests.
pub fn build_headers(
    config: &CodexApiConfig,
    user_agent: Option<&str>,
) -> Result<BTreeMap<String, String>, CodexApiError> {
    let mut headers = BTreeMap::new();

    if config.access_token.trim().is_empty() {
        return Err(CodexApiError::MissingAccessToken);
    }
    if config.account_id.trim().is_empty() {
        return Err(CodexApiError::MissingAccountId);
    }

    headers.insert(
        HEADER_AUTHORIZATION.to_owned(),
        format!("Bearer {}", config.access_token.trim()),
    );
    headers.insert(
        HEADER_ACCOUNT_ID.to_owned(),
        config.account_id.trim().to_owned(),
    );
    headers.insert(
        HEADER_ACCOUNT_ID_CANONICAL.to_owned(),
        config.account_id.trim().to_owned(),
    );
    headers.insert(
        HEADER_OPENAI_BETA.to_owned(),
        "responses=experimental".to_owned(),
    );
    headers.insert(
        HEADER_ORIGINATOR.to_owned(),
        config.originator.trim().to_owned(),
    );
    headers.insert(HEADER_ACCEPT.to_owned(), "text/event-stream".to_owned());
    headers.insert(
        HEADER_CONTENT_TYPE.to_owned(),
        "application/json".to_owned(),
    );

    let ua = match (user_agent, config.user_agent.as_deref()) {
        (Some(explicit), _) if !explicit.trim().is_empty() => explicit.trim().to_owned(),
        (None, Some(explicit)) if !explicit.trim().is_empty() => explicit.trim().to_owned(),
        _ => "codex-api/0.1.0".to_owned(),
    };
    headers.insert(HEADER_USER_AGENT.to_owned(), ua);

    if let Some(session_id) = &config.session_id {
        if !session_id.trim().is_empty() {
            headers.insert(HEADER_SESSION_ID.to_owned(), session_id.trim().to_owned());
        }
    }

    for (key, value) in &config.extra_headers {
        headers.insert(key.trim().to_lowercase(), value.trim().to_owned());
    }

    Ok(headers)
}
