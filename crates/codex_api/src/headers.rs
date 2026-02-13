use std::collections::BTreeMap;

use base64::{engine::general_purpose, Engine as _};
use serde::Deserialize;

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
    let account_id = resolve_account_id(config)?;

    headers.insert(
        HEADER_AUTHORIZATION.to_owned(),
        format!("Bearer {}", config.access_token.trim()),
    );
    headers.insert(HEADER_ACCOUNT_ID.to_owned(), account_id.clone());
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
        _ => default_pi_user_agent(),
    };
    headers.insert(HEADER_USER_AGENT.to_owned(), ua);

    for (key, value) in &config.extra_headers {
        headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    if let Some(session_id) = &config.session_id {
        if !session_id.trim().is_empty() {
            headers.insert(HEADER_SESSION_ID.to_owned(), session_id.trim().to_owned());
        }
    }

    Ok(headers)
}

fn resolve_account_id(config: &CodexApiConfig) -> Result<String, CodexApiError> {
    extract_account_id_from_token(config.access_token.trim()).ok_or(CodexApiError::MissingAccountId)
}

fn sanitize_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn extract_account_id_from_token(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload_segment = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = decode_jwt_segment(payload_segment)?;
    let claims = serde_json::from_slice::<TokenClaims>(&decoded).ok()?;

    claims
        .openai_auth
        .as_ref()
        .and_then(|auth| auth.chatgpt_account_id.as_deref())
        .and_then(sanitize_nonempty)
}

fn decode_jwt_segment(segment: &str) -> Option<Vec<u8>> {
    general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| general_purpose::URL_SAFE.decode(segment))
        .ok()
}

fn default_pi_user_agent() -> String {
    match runtime_os_triplet() {
        Some((platform, release, arch)) => format!("pi ({platform} {release}; {arch})"),
        None => "pi (browser)".to_owned(),
    }
}

fn normalize_arch(arch: &str) -> String {
    match arch.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" => "x64".to_owned(),
        "x86" | "i386" | "i686" => "ia32".to_owned(),
        "aarch64" => "arm64".to_owned(),
        normalized => normalized.to_owned(),
    }
}

#[cfg(unix)]
fn runtime_os_triplet() -> Option<(String, String, String)> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    let mut raw = MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: `uname` initializes the provided `utsname` struct on success.
    let rc = unsafe { libc::uname(raw.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }

    // SAFETY: We checked `uname` returned success, so `raw` is initialized.
    let raw = unsafe { raw.assume_init() };
    // SAFETY: `uname` provides NUL-terminated fixed-size C strings.
    let platform = unsafe { CStr::from_ptr(raw.sysname.as_ptr()) }
        .to_string_lossy()
        .to_lowercase();
    // SAFETY: `uname` provides NUL-terminated fixed-size C strings.
    let release = unsafe { CStr::from_ptr(raw.release.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: `uname` provides NUL-terminated fixed-size C strings.
    let arch = unsafe { CStr::from_ptr(raw.machine.as_ptr()) }.to_string_lossy();
    let arch = normalize_arch(&arch);

    if platform.is_empty() || release.is_empty() || arch.is_empty() {
        None
    } else {
        Some((platform, release, arch))
    }
}

#[cfg(not(unix))]
fn runtime_os_triplet() -> Option<(String, String, String)> {
    None
}

#[derive(Debug, Deserialize)]
struct TokenClaims {
    #[serde(rename = "https://api.openai.com/auth")]
    openai_auth: Option<OpenAiAuthClaims>,
}

#[derive(Debug, Deserialize)]
struct OpenAiAuthClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}
