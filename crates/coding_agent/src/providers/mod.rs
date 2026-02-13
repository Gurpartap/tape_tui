use std::fs;
use std::sync::Arc;
use std::time::Duration;

use agent_provider_codex_api::{CodexApiProvider, CodexApiProviderConfig, CODEX_API_PROVIDER_ID};
use agent_provider_mock::MOCK_PROVIDER_ID;
use serde::Deserialize;

use crate::provider::{ProviderInitError, RunProvider};

/// Environment variable used to select a run provider implementation.
pub const PROVIDER_ENV_VAR: &str = "CODING_AGENT_PROVIDER";
/// Environment variable containing a path to codex-api JSON bootstrap configuration.
pub const CODEX_CONFIG_PATH_ENV_VAR: &str = "CODING_AGENT_CODEX_CONFIG_PATH";
/// Provider IDs currently supported by this binary.
pub const SUPPORTED_PROVIDER_IDS: [&str; 2] = [MOCK_PROVIDER_ID, CODEX_API_PROVIDER_ID];

const ACCOUNT_ID_CLAIM_PATH: &str = "https://api.openai.com/auth.chatgpt_account_id";

pub use agent_provider_mock::MockProvider;

/// Resolves the configured run provider from explicit environment selection.
pub fn provider_from_env() -> Result<Arc<dyn RunProvider>, ProviderInitError> {
    let provider_id = std::env::var(PROVIDER_ENV_VAR).map_err(|_| {
        ProviderInitError::new(format!(
            "Missing provider selection. Set {PROVIDER_ENV_VAR} to one of: {}",
            supported_provider_list()
        ))
    })?;

    provider_for_id(provider_id.trim())
}

/// Resolves a run provider by provider ID.
pub fn provider_for_id(provider_id: &str) -> Result<Arc<dyn RunProvider>, ProviderInitError> {
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Err(ProviderInitError::new(format!(
            "Provider selection cannot be empty. Set {PROVIDER_ENV_VAR} to one of: {}",
            supported_provider_list()
        )));
    }

    match provider_id {
        MOCK_PROVIDER_ID => Ok(Arc::new(MockProvider::default())),
        CODEX_API_PROVIDER_ID => codex_api_provider_from_config_path_env(),
        unknown => Err(ProviderInitError::new(format!(
            "Unsupported provider '{unknown}'. Available providers: {}",
            supported_provider_list()
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexBootstrapConfig {
    access_token: String,
    models: Vec<String>,
    #[serde(default)]
    timeout_sec: Option<u64>,
}

fn codex_api_provider_from_config_path_env() -> Result<Arc<dyn RunProvider>, ProviderInitError> {
    let config_path = std::env::var(CODEX_CONFIG_PATH_ENV_VAR).map_err(|_| {
        ProviderInitError::new(format!(
            "Missing codex-api bootstrap config path. Set {CODEX_CONFIG_PATH_ENV_VAR} to a readable JSON file"
        ))
    })?;

    let config_path = config_path.trim();
    if config_path.is_empty() {
        return Err(ProviderInitError::new(format!(
            "{CODEX_CONFIG_PATH_ENV_VAR} cannot be empty; provide a readable JSON config path"
        )));
    }

    let raw_config = fs::read_to_string(config_path).map_err(|error| {
        ProviderInitError::new(format!(
            "Failed reading codex-api bootstrap config at '{config_path}' from {CODEX_CONFIG_PATH_ENV_VAR}: {error}"
        ))
    })?;

    let config: CodexBootstrapConfig = serde_json::from_str(&raw_config).map_err(|error| {
        ProviderInitError::new(format!(
            "Invalid codex-api bootstrap JSON at '{config_path}': {error}"
        ))
    })?;

    let access_token = sanitize_nonempty(config.access_token, "access_token")?;
    let models = sanitize_models(config.models)?;

    let mut provider_config = CodexApiProviderConfig::new(access_token, models);
    if let Some(timeout_sec) = config.timeout_sec {
        if timeout_sec == 0 {
            return Err(ProviderInitError::new(
                "codex-api bootstrap field 'timeout_sec' must be greater than zero when provided",
            ));
        }
        provider_config = provider_config.with_timeout(Duration::from_secs(timeout_sec));
    }

    CodexApiProvider::new(provider_config)
        .map(|provider| Arc::new(provider) as Arc<dyn RunProvider>)
        .map_err(|error| {
            if error.message().contains("account id is required") {
                ProviderInitError::new(format!(
                    "Invalid codex-api bootstrap token: access_token must be a JWT containing claim '{ACCOUNT_ID_CLAIM_PATH}'"
                ))
            } else {
                ProviderInitError::new(format!(
                    "Failed to initialize codex-api provider from '{config_path}': {}",
                    error.message()
                ))
            }
        })
}

fn sanitize_nonempty(value: String, field_name: &str) -> Result<String, ProviderInitError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ProviderInitError::new(format!(
            "codex-api bootstrap field '{field_name}' must be a non-empty string"
        )));
    }

    Ok(trimmed.to_string())
}

fn sanitize_models(models: Vec<String>) -> Result<Vec<String>, ProviderInitError> {
    let sanitized: Vec<String> = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();

    if sanitized.is_empty() {
        return Err(ProviderInitError::new(
            "codex-api bootstrap field 'models' must contain at least one non-empty model id",
        ));
    }

    Ok(sanitized)
}

fn supported_provider_list() -> String {
    SUPPORTED_PROVIDER_IDS.join(", ")
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use tempfile::NamedTempFile;

    use super::*;

    const VALID_ACCOUNT_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdC10ZXN0In19.sig";
    const MISSING_ACCOUNT_CLAIM_TOKEN: &str =
        "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.e30.sig";

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }

            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn write_bootstrap_config(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temp config file should be created");
        file.write_all(contents.as_bytes())
            .expect("temp config should be written");
        file
    }

    fn provider_init_error(
        result: Result<std::sync::Arc<dyn RunProvider>, ProviderInitError>,
        context: &str,
    ) -> ProviderInitError {
        match result {
            Ok(_) => panic!("{context}"),
            Err(error) => error,
        }
    }

    fn codex_bootstrap_json(access_token: &str, models: &str, timeout: Option<u64>) -> String {
        let timeout_fragment = timeout
            .map(|value| format!(",\n  \"timeout_sec\": {value}"))
            .unwrap_or_default();

        format!(
            "{{\n  \"access_token\": \"{access_token}\",\n  \"models\": {models}{timeout_fragment}\n}}"
        )
    }

    #[test]
    fn provider_for_id_supports_mock() {
        let provider = provider_for_id(MOCK_PROVIDER_ID).expect("mock provider should resolve");
        assert_eq!(provider.profile().provider_id, MOCK_PROVIDER_ID);
    }

    #[test]
    fn provider_for_id_rejects_unknown_provider() {
        let error = match provider_for_id("custom") {
            Ok(_) => panic!("unknown providers should fail"),
            Err(error) => error,
        };

        assert!(error.message().contains("Unsupported provider 'custom'"));
        assert!(error.message().contains(MOCK_PROVIDER_ID));
        assert!(error.message().contains(CODEX_API_PROVIDER_ID));
    }

    #[test]
    fn provider_from_env_requires_explicit_provider_id() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, None);

        let error = match provider_from_env() {
            Ok(_) => panic!("provider selection should be required"),
            Err(error) => error,
        };
        assert!(error
            .message()
            .contains("Missing provider selection. Set CODING_AGENT_PROVIDER"));
    }

    #[test]
    fn provider_from_env_rejects_empty_provider_id() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some("  \t  "));

        let error = match provider_from_env() {
            Ok(_) => panic!("empty provider selection should fail"),
            Err(error) => error,
        };
        assert!(error
            .message()
            .contains("Provider selection cannot be empty"));
    }

    #[test]
    fn provider_from_env_resolves_explicit_provider_id() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some("  mock  "));

        let provider = provider_from_env().expect("explicit mock provider should resolve");
        assert_eq!(provider.profile().provider_id, MOCK_PROVIDER_ID);
    }

    #[test]
    fn codex_bootstrap_requires_config_path_env() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(CODEX_CONFIG_PATH_ENV_VAR, None);

        let error = provider_init_error(provider_from_env(), "missing config path should fail");
        assert!(error
            .message()
            .contains("Missing codex-api bootstrap config path"));
    }

    #[test]
    fn codex_bootstrap_rejects_unreadable_path() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some("/tmp/coding-agent-missing-bootstrap-config.json"),
        );

        let error = provider_init_error(provider_from_env(), "unreadable config path should fail");
        assert!(error
            .message()
            .contains("Failed reading codex-api bootstrap config"));
    }

    #[test]
    fn codex_bootstrap_rejects_invalid_json() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config("{");
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "invalid JSON should fail");
        assert!(error.message().contains("Invalid codex-api bootstrap JSON"));
    }

    #[test]
    fn codex_bootstrap_rejects_unknown_field() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&format!(
            "{{\n  \"access_token\": \"{VALID_ACCOUNT_TOKEN}\",\n  \"models\": [\"gpt-5.3-codex\"],\n  \"unknown\": true\n}}"
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "unknown fields should fail");
        assert!(error.message().contains("unknown field `unknown`"));
    }

    #[test]
    fn codex_bootstrap_rejects_missing_required_fields() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&format!(
            "{{\n  \"access_token\": \"{VALID_ACCOUNT_TOKEN}\"\n}}"
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "missing required fields should fail");
        assert!(error.message().contains("missing field `models`"));
    }

    #[test]
    fn codex_bootstrap_rejects_empty_token_and_models() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&codex_bootstrap_json("   ", "[]", None));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "empty token should fail before model checks");
        assert!(error.message().contains("field 'access_token'"));
    }

    #[test]
    fn codex_bootstrap_rejects_empty_models_after_trimming() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&codex_bootstrap_json(
            VALID_ACCOUNT_TOKEN,
            "[\"  \", \"\"]",
            None,
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "blank models should fail");
        assert!(error.message().contains("field 'models'"));
    }

    #[test]
    fn codex_bootstrap_rejects_zero_timeout() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&codex_bootstrap_json(
            VALID_ACCOUNT_TOKEN,
            "[\"gpt-5.3-codex\"]",
            Some(0),
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "zero timeout should fail");
        assert!(error.message().contains("'timeout_sec' must be greater than zero"));
    }

    #[test]
    fn codex_bootstrap_rejects_token_without_account_claim() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&codex_bootstrap_json(
            MISSING_ACCOUNT_CLAIM_TOKEN,
            "[\"gpt-5.3-codex\"]",
            None,
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let error = provider_init_error(provider_from_env(), "token without account claim should fail");
        assert!(error
            .message()
            .contains("must be a JWT containing claim 'https://api.openai.com/auth.chatgpt_account_id'"));
    }

    #[test]
    fn codex_bootstrap_happy_path_initializes_provider() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let file = write_bootstrap_config(&codex_bootstrap_json(
            VALID_ACCOUNT_TOKEN,
            "[\"gpt-5.3-codex\"]",
            Some(120),
        ));
        let _provider = EnvVarGuard::set(PROVIDER_ENV_VAR, Some(CODEX_API_PROVIDER_ID));
        let _config = EnvVarGuard::set(
            CODEX_CONFIG_PATH_ENV_VAR,
            Some(file.path().to_str().expect("temp path must be utf-8")),
        );

        let provider = provider_from_env().expect("valid bootstrap should initialize codex provider");
        assert_eq!(provider.profile().provider_id, CODEX_API_PROVIDER_ID);
        assert_eq!(provider.profile().model_id, "gpt-5.3-codex");
    }
}
