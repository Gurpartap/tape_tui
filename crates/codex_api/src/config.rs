use std::collections::BTreeMap;
use std::time::Duration;

use crate::url::DEFAULT_CODEX_BASE_URL;

/// Transport configuration for Codex API requests.
#[derive(Debug, Clone)]
pub struct CodexApiConfig {
    /// OAuth/bearer token passed to `Authorization`.
    pub access_token: String,
    /// Account identifier carried in `chatgpt-account-id` header family.
    pub account_id: String,
    /// Base URL for Codex endpoints.
    pub base_url: String,
    /// Client-origin identifier added to outgoing headers.
    pub originator: String,
    /// Optional `session_id` request header value.
    pub session_id: Option<String>,
    /// Optional `User-Agent` override.
    pub user_agent: Option<String>,
    /// Additional headers merged into request headers.
    pub extra_headers: BTreeMap<String, String>,
    /// Optional request timeout.
    pub timeout: Option<Duration>,
}

impl Default for CodexApiConfig {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            account_id: String::new(),
            base_url: DEFAULT_CODEX_BASE_URL.to_string(),
            originator: "pi".to_string(),
            session_id: None,
            user_agent: None,
            extra_headers: BTreeMap::new(),
            timeout: None,
        }
    }
}

impl CodexApiConfig {
    pub fn new(access_token: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
            account_id: account_id.into(),
            ..Self::default()
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_originator(mut self, originator: impl Into<String>) -> Self {
        self.originator = originator.into();
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn insert_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.insert(key.into(), value.into());
        self
    }

    pub fn with_headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Self {
        self.extra_headers.extend(headers);
        self
    }
}
