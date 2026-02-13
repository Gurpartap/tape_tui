use std::sync::Arc;

use crate::provider::RunProvider;

mod mock;

pub use mock::MockProvider;

pub const DEFAULT_PROVIDER_ID: &str = "mock";
pub const PROVIDER_ENV_VAR: &str = "CODING_AGENT_PROVIDER";

pub fn provider_from_env() -> Result<Arc<dyn RunProvider>, String> {
    let provider_id = std::env::var(PROVIDER_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    provider_for_id(provider_id.as_deref().unwrap_or(DEFAULT_PROVIDER_ID))
}

pub fn provider_for_id(provider_id: &str) -> Result<Arc<dyn RunProvider>, String> {
    match provider_id {
        DEFAULT_PROVIDER_ID => Ok(Arc::new(MockProvider::default())),
        unknown => Err(format!(
            "Unsupported provider '{unknown}'. Available providers: {DEFAULT_PROVIDER_ID}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_for_id_supports_mock() {
        let provider = provider_for_id("mock").expect("mock provider should resolve");
        assert_eq!(provider.profile().provider_id, "mock");
    }

    #[test]
    fn provider_for_id_rejects_unknown_provider() {
        let error = match provider_for_id("custom") {
            Ok(_) => panic!("unknown providers should fail"),
            Err(error) => error,
        };

        assert!(error.contains("Unsupported provider 'custom'"));
    }
}
