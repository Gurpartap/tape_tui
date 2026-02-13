//! Codex API-backed implementation of the shared `agent_provider` contract.
//!
//! This adapter translates `codex_api` stream semantics into deterministic
//! `RunEvent` lifecycle events expected by `coding_agent`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_provider::{
    CancelSignal, ProviderInitError, ProviderProfile, RunEvent, RunProvider, RunRequest,
};
use codex_api::{
    CodexApiClient, CodexApiConfig, CodexApiError, CodexRequest, CodexResponseStatus,
    CodexStreamEvent, StreamResult,
};

/// Stable provider identifier used by `coding_agent` startup selection.
pub const CODEX_API_PROVIDER_ID: &str = "codex-api";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionState {
    model_index: usize,
}

/// Runtime configuration for the Codex API provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexApiProviderConfig {
    pub access_token: String,
    pub model_ids: Vec<String>,
    pub base_url: Option<String>,
    pub session_id: Option<String>,
    pub timeout: Option<Duration>,
}

impl CodexApiProviderConfig {
    #[must_use]
    pub fn new(access_token: impl Into<String>, model_ids: Vec<String>) -> Self {
        Self {
            access_token: access_token.into(),
            model_ids,
            base_url: None,
            session_id: None,
            timeout: None,
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn into_codex_api_config(self) -> CodexApiConfig {
        let mut config = CodexApiConfig::new(self.access_token);

        if let Some(base_url) = self.base_url {
            config = config.with_base_url(base_url);
        }

        if let Some(session_id) = self.session_id {
            config = config.with_session_id(session_id);
        }

        if let Some(timeout) = self.timeout {
            config = config.with_timeout(timeout);
        }

        config
    }
}

trait StreamClient: Send + Sync {
    fn stream(
        &self,
        request: &CodexRequest,
        cancel: &CancelSignal,
    ) -> Result<StreamResult, CodexApiError>;
}

#[derive(Debug)]
struct DefaultStreamClient {
    client: CodexApiClient,
}

impl StreamClient for DefaultStreamClient {
    fn stream(
        &self,
        request: &CodexRequest,
        cancel: &CancelSignal,
    ) -> Result<StreamResult, CodexApiError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                CodexApiError::Unknown(format!("failed to initialize tokio runtime: {error}"))
            })?;

        runtime.block_on(self.client.stream(request, Some(cancel)))
    }
}

/// `RunProvider` adapter backed by `codex_api` transport primitives.
pub struct CodexApiProvider {
    model_ids: Vec<String>,
    selection: Mutex<SelectionState>,
    stream_client: Arc<dyn StreamClient>,
}

impl CodexApiProvider {
    /// Creates a provider using real Codex API transport.
    pub fn new(config: CodexApiProviderConfig) -> Result<Self, ProviderInitError> {
        let model_ids = sanitize_model_ids(config.model_ids.clone());
        let stream_client = Arc::new(DefaultStreamClient {
            client: CodexApiClient::new(config.into_codex_api_config()).map_err(map_init_error)?,
        });

        Ok(Self {
            model_ids,
            selection: Mutex::new(SelectionState { model_index: 0 }),
            stream_client,
        })
    }

    fn selected_model(&self) -> String {
        let selection = lock_unpoisoned(&self.selection);
        self.model_ids[selection.model_index].clone()
    }

    fn emit_stream_chunks(&self, run_id: u64, stream_events: Vec<CodexStreamEvent>, emit: &mut dyn FnMut(RunEvent)) {
        for stream_event in stream_events {
            match stream_event {
                CodexStreamEvent::OutputTextDelta { delta }
                | CodexStreamEvent::ReasoningSummaryTextDelta { delta } => {
                    if !delta.is_empty() {
                        emit(RunEvent::Chunk {
                            run_id,
                            text: delta,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn emit_terminal_event(
        &self,
        run_id: u64,
        terminal: Option<CodexResponseStatus>,
        emit: &mut dyn FnMut(RunEvent),
    ) {
        match terminal {
            Some(CodexResponseStatus::Completed) => emit(RunEvent::Finished { run_id }),
            Some(CodexResponseStatus::Cancelled) => emit(RunEvent::Cancelled { run_id }),
            Some(CodexResponseStatus::Failed) => emit(RunEvent::Failed {
                run_id,
                error: "Codex API response failed".to_string(),
            }),
            Some(status) => emit(RunEvent::Failed {
                run_id,
                error: format!(
                    "Codex API response ended with non-complete terminal status '{}'",
                    status.as_str()
                ),
            }),
            None => emit(RunEvent::Failed {
                run_id,
                error: "Codex API stream ended without terminal status".to_string(),
            }),
        }
    }

    #[cfg(test)]
    fn with_stream_client_for_tests(
        model_ids: Vec<String>,
        stream_client: Arc<dyn StreamClient>,
    ) -> Self {
        Self {
            model_ids: sanitize_model_ids(model_ids),
            selection: Mutex::new(SelectionState { model_index: 0 }),
            stream_client,
        }
    }
}

impl RunProvider for CodexApiProvider {
    fn profile(&self) -> ProviderProfile {
        ProviderProfile {
            provider_id: CODEX_API_PROVIDER_ID.to_string(),
            model_id: self.selected_model(),
            thinking_level: None,
        }
    }

    fn cycle_model(&self) -> Result<ProviderProfile, String> {
        let mut selection = lock_unpoisoned(&self.selection);
        selection.model_index = (selection.model_index + 1) % self.model_ids.len();
        drop(selection);

        Ok(self.profile())
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let run_id = req.run_id;

        emit(RunEvent::Started { run_id });

        if cancel.load(Ordering::Acquire) {
            emit(RunEvent::Cancelled { run_id });
            return Ok(());
        }

        let request = CodexRequest::new(self.selected_model(), req.prompt, None);
        match self.stream_client.stream(&request, &cancel) {
            Ok(result) => {
                self.emit_stream_chunks(run_id, result.events, emit);
                self.emit_terminal_event(run_id, result.terminal, emit);
            }
            Err(CodexApiError::Cancelled) => emit(RunEvent::Cancelled { run_id }),
            Err(error) => emit(RunEvent::Failed {
                run_id,
                error: format!("Codex API request failed: {error}"),
            }),
        }

        Ok(())
    }
}

fn sanitize_model_ids(model_ids: Vec<String>) -> Vec<String> {
    let mut sanitized: Vec<String> = model_ids
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();

    if sanitized.is_empty() {
        sanitized.push("gpt-5.1-codex".to_string());
    }

    sanitized
}

fn map_init_error(error: CodexApiError) -> ProviderInitError {
    ProviderInitError::new(format!("Failed to initialize codex-api provider: {error}"))
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    enum FakeStreamOutcome {
        Success(StreamResult),
        Error(CodexApiError),
    }

    struct FakeStreamClient {
        observed_model: Mutex<Option<String>>,
        outcome: Mutex<Option<FakeStreamOutcome>>,
    }

    impl FakeStreamClient {
        fn success(result: StreamResult) -> Arc<Self> {
            Arc::new(Self {
                observed_model: Mutex::new(None),
                outcome: Mutex::new(Some(FakeStreamOutcome::Success(result))),
            })
        }

        fn failure(error: CodexApiError) -> Arc<Self> {
            Arc::new(Self {
                observed_model: Mutex::new(None),
                outcome: Mutex::new(Some(FakeStreamOutcome::Error(error))),
            })
        }

        fn observed_model(&self) -> Option<String> {
            lock_unpoisoned(&self.observed_model).clone()
        }
    }

    impl StreamClient for FakeStreamClient {
        fn stream(
            &self,
            request: &CodexRequest,
            _cancel: &CancelSignal,
        ) -> Result<StreamResult, CodexApiError> {
            *lock_unpoisoned(&self.observed_model) = Some(request.model.clone());

            match lock_unpoisoned(&self.outcome).take() {
                Some(FakeStreamOutcome::Success(result)) => Ok(result),
                Some(FakeStreamOutcome::Error(error)) => Err(error),
                None => panic!("fake stream outcome should be consumed exactly once"),
            }
        }
    }

    fn run_events(provider: &CodexApiProvider) -> Vec<RunEvent> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();

        provider
            .run(
                RunRequest {
                    run_id: 9,
                    prompt: "hello".to_string(),
                },
                cancel,
                &mut |event| events.push(event),
            )
            .expect("run should not return provider-level failure");

        events
    }

    #[test]
    fn profile_reports_codex_provider_id_and_selected_model() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string(), "gpt-5.2-codex".to_string()],
            stream,
        );

        let initial = provider.profile();
        assert_eq!(initial.provider_id, CODEX_API_PROVIDER_ID);
        assert_eq!(initial.model_id, "gpt-5.1-codex");

        let switched = provider
            .cycle_model()
            .expect("codex provider should support model cycling");
        assert_eq!(switched.model_id, "gpt-5.2-codex");
    }

    #[test]
    fn run_maps_stream_deltas_to_chunks_and_completed_to_finished() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![
                CodexStreamEvent::OutputTextDelta {
                    delta: "Hello".to_string(),
                },
                CodexStreamEvent::ReasoningSummaryTextDelta {
                    delta: " world".to_string(),
                },
            ],
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events(&provider);

        assert_eq!(stream.observed_model().as_deref(), Some("gpt-5.1-codex"));
        assert!(matches!(events.first(), Some(RunEvent::Started { run_id: 9 })));
        assert!(events
            .iter()
            .any(|event| matches!(event, RunEvent::Chunk { text, .. } if text == "Hello")));
        assert!(events
            .iter()
            .any(|event| matches!(event, RunEvent::Chunk { text, .. } if text == " world")));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_maps_cancelled_transport_to_cancelled_terminal_event() {
        let stream = FakeStreamClient::failure(CodexApiError::Cancelled);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            stream,
        );

        let events = run_events(&provider);

        assert!(matches!(events.first(), Some(RunEvent::Started { run_id: 9 })));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Cancelled { run_id: 9 })
        ));
    }

    #[test]
    fn run_maps_transport_error_to_failed_terminal_event() {
        let stream = FakeStreamClient::failure(CodexApiError::Unknown("boom".to_string()));
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            stream,
        );

        let events = run_events(&provider);

        assert!(matches!(events.first(), Some(RunEvent::Started { run_id: 9 })));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("boom")
        ));
    }

    #[test]
    fn run_maps_non_complete_terminal_status_to_failed_event() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::InProgress),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            stream,
        );

        let events = run_events(&provider);

        assert!(matches!(events.first(), Some(RunEvent::Started { run_id: 9 })));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("in_progress")
        ));
    }

    #[test]
    fn empty_model_list_defaults_to_safe_codex_model() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(Vec::new(), stream);

        assert_eq!(provider.profile().model_id, "gpt-5.1-codex");
    }
}
