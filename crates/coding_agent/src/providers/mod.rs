use std::sync::Arc;

use agent_provider_mock::MOCK_PROVIDER_ID;

use crate::provider::{ProviderInitError, RunProvider};

/// Environment variable used to select a run provider implementation.
pub const PROVIDER_ENV_VAR: &str = "CODING_AGENT_PROVIDER";
/// Provider IDs currently supported by this binary.
pub const SUPPORTED_PROVIDER_IDS: [&str; 1] = [MOCK_PROVIDER_ID];

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
        unknown => Err(ProviderInitError::new(format!(
            "Unsupported provider '{unknown}'. Available providers: {}",
            supported_provider_list()
        ))),
    }
}

fn supported_provider_list() -> String {
    SUPPORTED_PROVIDER_IDS.join(", ")
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;

    struct EnvVarGuard {
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var(PROVIDER_ENV_VAR).ok();
            match value {
                Some(value) => std::env::set_var(PROVIDER_ENV_VAR, value),
                None => std::env::remove_var(PROVIDER_ENV_VAR),
            }

            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(PROVIDER_ENV_VAR, value),
                None => std::env::remove_var(PROVIDER_ENV_VAR),
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
    }

    #[test]
    fn provider_from_env_requires_explicit_provider_id() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _guard = EnvVarGuard::set(None);

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
        let _guard = EnvVarGuard::set(Some("  \t  "));

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
        let _guard = EnvVarGuard::set(Some("  mock  "));

        let provider = provider_from_env().expect("explicit mock provider should resolve");
        assert_eq!(provider.profile().provider_id, MOCK_PROVIDER_ID);
    }
}
