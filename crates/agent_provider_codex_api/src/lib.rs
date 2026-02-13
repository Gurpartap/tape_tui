//! Codex API-backed implementation of the shared `agent_provider` contract.
//!
//! This adapter translates `codex_api` stream semantics into deterministic
//! `RunEvent` lifecycle events expected by `coding_agent`.
//! Host-mediated tool execution is serial and limited to the v1 tool pack
//! (`bash`, `read`, `edit`, `write`), with explicit failure/cancel outcomes for
//! malformed payloads or non-complete terminal statuses.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_provider::{
    CancelSignal, ProviderInitError, ProviderProfile, RunEvent, RunProvider, RunRequest,
    ToolCallRequest, ToolDefinition, ToolResult,
};
use codex_api::{
    normalize_codex_url, CodexApiClient, CodexApiConfig, CodexApiError, CodexRequest,
    CodexResponseStatus, CodexStreamEvent, StreamResult,
};
use serde_json::{json, Value};
use url::Url;

/// Stable provider identifier used by `coding_agent` startup selection.
pub const CODEX_API_PROVIDER_ID: &str = "codex-api";

const V1_TOOL_NAMES: [&str; 4] = ["bash", "read", "edit", "write"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionState {
    model_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingToolCall {
    call_id: String,
    tool_name: String,
    arguments: Value,
    arguments_json: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ToolRoundtrip {
    pending_call: PendingToolCall,
    result: ToolResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedConfig {
    access_token: String,
    model_ids: Vec<String>,
    base_url: Option<String>,
    session_id: Option<String>,
    timeout: Option<Duration>,
}

impl ValidatedConfig {
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

    fn validate(self) -> Result<ValidatedConfig, ProviderInitError> {
        let access_token = sanitize_required_string(self.access_token, "access token")?;
        let model_ids = sanitize_model_ids(self.model_ids)?;
        let base_url = sanitize_optional_string(self.base_url, "base URL")?;
        let session_id = sanitize_optional_string(self.session_id, "session id")?;

        if let Some(timeout) = self.timeout {
            if timeout.is_zero() {
                return Err(ProviderInitError::new(
                    "codex-api provider timeout must be greater than zero when provided",
                ));
            }
        }

        if let Some(base_url) = base_url.as_deref() {
            let endpoint = normalize_codex_url(base_url);
            Url::parse(&endpoint).map_err(|error| {
                ProviderInitError::new(format!("codex-api provider base URL is invalid: {error}"))
            })?;
        }

        Ok(ValidatedConfig {
            access_token,
            model_ids,
            base_url,
            session_id,
            timeout: self.timeout,
        })
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
        let validated = config.validate()?;
        let model_ids = validated.model_ids.clone();

        let client =
            CodexApiClient::new(validated.into_codex_api_config()).map_err(map_init_error)?;
        client.build_headers(None).map_err(map_init_error)?;

        let stream_client = Arc::new(DefaultStreamClient { client });

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

    fn process_stream_events(
        &self,
        run_id: u64,
        stream_events: Vec<CodexStreamEvent>,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<Vec<PendingToolCall>, String> {
        let mut pending_tool_calls = Vec::new();

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
                CodexStreamEvent::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                    ..
                } => {
                    pending_tool_calls
                        .push(parse_pending_tool_call(call_id, tool_name, arguments)?);
                }
                _ => {}
            }
        }

        Ok(pending_tool_calls)
    }

    fn build_initial_request(&self, model_id: &str, prompt: String) -> CodexRequest {
        let mut request = CodexRequest::new(model_id.to_owned(), prompt, None);
        request.tools = codex_tool_payloads();
        request
    }

    fn build_follow_up_request(
        &self,
        model_id: &str,
        roundtrips: &[ToolRoundtrip],
    ) -> CodexRequest {
        let mut input = Vec::with_capacity(roundtrips.len() * 2);

        for roundtrip in roundtrips {
            input.push(json!({
                "type": "function_call",
                "call_id": roundtrip.pending_call.call_id.clone(),
                "name": roundtrip.pending_call.tool_name.clone(),
                "arguments": roundtrip.pending_call.arguments_json.clone(),
            }));
        }

        for roundtrip in roundtrips {
            input.push(json!({
                "type": "function_call_output",
                "call_id": roundtrip.result.call_id.clone(),
                "output": codex_tool_output_payload(&roundtrip.result),
            }));
        }

        let mut request = CodexRequest::new(model_id.to_owned(), Value::Array(input), None);
        request.tools = codex_tool_payloads();
        request
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
        let model_ids = sanitize_model_ids(model_ids)
            .expect("tests must provide at least one non-empty model id");

        Self {
            model_ids,
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

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        v1_tool_definitions()
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
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let run_id = req.run_id;
        let model_id = self.selected_model();

        emit(RunEvent::Started { run_id });

        if cancel.load(Ordering::Acquire) {
            emit(RunEvent::Cancelled { run_id });
            return Ok(());
        }

        let mut request = self.build_initial_request(&model_id, req.prompt);

        loop {
            if cancel.load(Ordering::Acquire) {
                emit(RunEvent::Cancelled { run_id });
                return Ok(());
            }

            let stream_result = match self.stream_client.stream(&request, &cancel) {
                Ok(result) => result,
                Err(CodexApiError::Cancelled) => {
                    emit(RunEvent::Cancelled { run_id });
                    return Ok(());
                }
                Err(error) => {
                    emit(RunEvent::Failed {
                        run_id,
                        error: format!("Codex API request failed: {error}"),
                    });
                    return Ok(());
                }
            };

            let pending_calls = match self.process_stream_events(run_id, stream_result.events, emit)
            {
                Ok(calls) => calls,
                Err(error) => {
                    emit(RunEvent::Failed { run_id, error });
                    return Ok(());
                }
            };

            if pending_calls.is_empty() {
                self.emit_terminal_event(run_id, stream_result.terminal, emit);
                return Ok(());
            }

            match stream_result.terminal {
                Some(CodexResponseStatus::Completed) => {}
                Some(CodexResponseStatus::Cancelled) => {
                    emit(RunEvent::Cancelled { run_id });
                    return Ok(());
                }
                Some(status) => {
                    emit(RunEvent::Failed {
                        run_id,
                        error: format!(
                            "Codex API response ended with non-complete terminal status '{}' while processing tool calls",
                            status.as_str()
                        ),
                    });
                    return Ok(());
                }
                None => {
                    emit(RunEvent::Failed {
                        run_id,
                        error: "Codex API stream ended without terminal status while processing tool calls"
                            .to_string(),
                    });
                    return Ok(());
                }
            }

            let mut roundtrips = Vec::with_capacity(pending_calls.len());
            for pending_call in pending_calls {
                if cancel.load(Ordering::Acquire) {
                    emit(RunEvent::Cancelled { run_id });
                    return Ok(());
                }

                let result = execute_tool(ToolCallRequest {
                    call_id: pending_call.call_id.clone(),
                    tool_name: pending_call.tool_name.clone(),
                    arguments: pending_call.arguments.clone(),
                });

                roundtrips.push(ToolRoundtrip {
                    pending_call,
                    result,
                });
            }

            request = self.build_follow_up_request(&model_id, &roundtrips);
        }
    }
}

fn v1_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "bash".to_string(),
            description: Some("Execute a shell command in the current workspace".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_sec": { "type": "integer", "minimum": 1 },
                    "cwd": { "type": "string" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "read".to_string(),
            description: Some("Read UTF-8 text from a workspace-relative file".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "edit".to_string(),
            description: Some("Replace exact text within a workspace file".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" }
                },
                "required": ["path", "old_text", "new_text"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "write".to_string(),
            description: Some("Write UTF-8 text content to a workspace file".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
    ]
}

fn codex_tool_payloads() -> Vec<Value> {
    v1_tool_definitions()
        .into_iter()
        .map(|definition| {
            let mut tool = json!({
                "type": "function",
                "name": definition.name,
                "parameters": definition.input_schema,
            });

            if let Some(description) = definition.description {
                tool["description"] = Value::String(description);
            }

            tool
        })
        .collect()
}

fn parse_pending_tool_call(
    call_id: Option<String>,
    tool_name: Option<String>,
    arguments: Option<Value>,
) -> Result<PendingToolCall, String> {
    let call_id = required_stream_string(call_id, "call_id")?;
    let tool_name = required_stream_string(tool_name, "tool_name")?;

    if !V1_TOOL_NAMES.contains(&tool_name.as_str()) {
        return Err(format!(
            "Unsupported tool call '{tool_name}' from Codex API; supported tools: {}",
            V1_TOOL_NAMES.join(", ")
        ));
    }

    let arguments = arguments.ok_or_else(|| {
        format!("Malformed tool call payload for '{tool_name}': missing arguments",)
    })?;

    let (arguments, arguments_json) = normalize_tool_arguments(&tool_name, arguments)?;

    Ok(PendingToolCall {
        call_id,
        tool_name,
        arguments,
        arguments_json,
    })
}

fn required_stream_string(value: Option<String>, field_name: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| {
        format!("Malformed tool call payload: missing required field '{field_name}'",)
    })?;

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "Malformed tool call payload: field '{field_name}' cannot be empty",
        ));
    }

    Ok(trimmed.to_string())
}

fn normalize_tool_arguments(tool_name: &str, arguments: Value) -> Result<(Value, String), String> {
    match arguments {
        Value::String(arguments_json) => {
            let parsed = serde_json::from_str::<Value>(&arguments_json).map_err(|error| {
                format!(
                    "Malformed tool call payload for '{tool_name}': arguments must be valid JSON ({error})",
                )
            })?;

            if !parsed.is_object() {
                return Err(format!(
                    "Malformed tool call payload for '{tool_name}': arguments must decode to a JSON object",
                ));
            }

            Ok((parsed, arguments_json))
        }
        Value::Object(_) => {
            let arguments_json = arguments.to_string();
            Ok((arguments, arguments_json))
        }
        other => Err(format!(
            "Malformed tool call payload for '{tool_name}': arguments must be a JSON object or string, got {}",
            value_type_name(&other)
        )),
    }
}

fn codex_tool_output_payload(result: &ToolResult) -> Value {
    let content_text = tool_result_content_text(&result.content);
    if result.is_error {
        json!({
            "content": content_text,
            "success": false,
        })
    } else {
        Value::String(content_text)
    }
}

fn tool_result_content_text(value: &Value) -> String {
    match value {
        Value::String(content) => content.clone(),
        other => other.to_string(),
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn sanitize_required_string(value: String, field_name: &str) -> Result<String, ProviderInitError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ProviderInitError::new(format!(
            "codex-api provider requires a non-empty {field_name}",
        )));
    }

    Ok(trimmed.to_string())
}

fn sanitize_optional_string(
    value: Option<String>,
    field_name: &str,
) -> Result<Option<String>, ProviderInitError> {
    match value {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(ProviderInitError::new(format!(
                    "codex-api provider field '{field_name}' cannot be empty when provided",
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

fn sanitize_model_ids(model_ids: Vec<String>) -> Result<Vec<String>, ProviderInitError> {
    let sanitized: Vec<String> = model_ids
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();

    if sanitized.is_empty() {
        return Err(ProviderInitError::new(
            "codex-api provider requires at least one non-empty model id",
        ));
    }

    Ok(sanitized)
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
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[derive(Debug)]
    enum FakeStreamOutcome {
        Success(StreamResult),
        Error(CodexApiError),
    }

    struct FakeStreamClient {
        observed_requests: Mutex<Vec<CodexRequest>>,
        outcomes: Mutex<VecDeque<FakeStreamOutcome>>,
    }

    impl FakeStreamClient {
        fn scripted(outcomes: Vec<FakeStreamOutcome>) -> Arc<Self> {
            Arc::new(Self {
                observed_requests: Mutex::new(Vec::new()),
                outcomes: Mutex::new(VecDeque::from(outcomes)),
            })
        }

        fn success(result: StreamResult) -> Arc<Self> {
            Self::scripted(vec![FakeStreamOutcome::Success(result)])
        }

        fn failure(error: CodexApiError) -> Arc<Self> {
            Self::scripted(vec![FakeStreamOutcome::Error(error)])
        }

        fn observed_requests(&self) -> Vec<CodexRequest> {
            lock_unpoisoned(&self.observed_requests).clone()
        }
    }

    impl StreamClient for FakeStreamClient {
        fn stream(
            &self,
            request: &CodexRequest,
            _cancel: &CancelSignal,
        ) -> Result<StreamResult, CodexApiError> {
            lock_unpoisoned(&self.observed_requests).push(request.clone());

            match lock_unpoisoned(&self.outcomes).pop_front() {
                Some(FakeStreamOutcome::Success(result)) => Ok(result),
                Some(FakeStreamOutcome::Error(error)) => Err(error),
                None => panic!("fake stream outcomes should cover every adapter request"),
            }
        }
    }

    fn run_events_with_executor(
        provider: &CodexApiProvider,
        mut execute_tool: impl FnMut(ToolCallRequest) -> ToolResult,
    ) -> Vec<RunEvent> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();

        provider
            .run(
                RunRequest {
                    run_id: 9,
                    prompt: "hello".to_string(),
                },
                cancel,
                &mut execute_tool,
                &mut |event| events.push(event),
            )
            .expect("run should not return provider-level failure");

        events
    }

    fn run_events(provider: &CodexApiProvider) -> Vec<RunEvent> {
        run_events_with_executor(provider, |_call| {
            ToolResult::error("unused", "unused", "not used in this test")
        })
    }

    fn init_error(config: CodexApiProviderConfig) -> ProviderInitError {
        match CodexApiProvider::new(config) {
            Ok(_) => panic!("provider init should fail for this test case"),
            Err(error) => error,
        }
    }

    fn request_tool_names(request: &CodexRequest) -> Vec<String> {
        request
            .tools
            .iter()
            .filter_map(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect()
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
    fn tool_definitions_advertise_only_v1_tools() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let names: Vec<String> = provider
            .tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();

        assert_eq!(
            names,
            V1_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>()
        );
        assert!(!names.iter().any(|name| name == "apply_patch"));
    }

    #[test]
    fn run_without_tool_calls_emits_chunks_and_finishes() {
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

        let events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not be invoked when no tool calls are streamed")
        });

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "gpt-5.1-codex");
        assert_eq!(
            request_tool_names(&requests[0]),
            V1_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>()
        );

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 9 })
        ));
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
    fn run_performs_single_tool_call_roundtrip() {
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![CodexStreamEvent::ToolCallRequested {
                    id: Some("fc_1".to_string()),
                    call_id: Some("call_1".to_string()),
                    tool_name: Some("read".to_string()),
                    arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
                }],
                terminal: Some(CodexResponseStatus::Completed),
            }),
            FakeStreamOutcome::Success(StreamResult {
                events: vec![CodexStreamEvent::OutputTextDelta {
                    delta: "done".to_string(),
                }],
                terminal: Some(CodexResponseStatus::Completed),
            }),
        ]);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut observed_calls = Vec::new();
        let events = run_events_with_executor(&provider, |call| {
            observed_calls.push(call.clone());
            ToolResult::success(call.call_id, call.tool_name, "file contents")
        });

        assert_eq!(observed_calls.len(), 1);
        assert_eq!(observed_calls[0].tool_name, "read");
        assert_eq!(observed_calls[0].arguments["path"], "README.md");

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input.len(), 2);
        assert_eq!(follow_up_input[0]["type"], "function_call");
        assert_eq!(follow_up_input[0]["call_id"], "call_1");
        assert_eq!(follow_up_input[0]["name"], "read");
        assert_eq!(follow_up_input[1]["type"], "function_call_output");
        assert_eq!(follow_up_input[1]["call_id"], "call_1");
        assert_eq!(follow_up_input[1]["output"], "file contents");

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_processes_multiple_tool_calls_in_serial_order() {
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_1".to_string()),
                        call_id: Some("call_1".to_string()),
                        tool_name: Some("bash".to_string()),
                        arguments: Some(Value::String("{\"command\":\"pwd\"}".to_string())),
                    },
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_2".to_string()),
                        call_id: Some("call_2".to_string()),
                        tool_name: Some("read".to_string()),
                        arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
                    },
                ],
                terminal: Some(CodexResponseStatus::Completed),
            }),
            FakeStreamOutcome::Success(StreamResult {
                events: vec![],
                terminal: Some(CodexResponseStatus::Completed),
            }),
        ]);

        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut call_order = Vec::new();
        let events = run_events_with_executor(&provider, |call| {
            call_order.push(call.call_id.clone());
            ToolResult::success(
                call.call_id,
                call.tool_name,
                format!("ok:{}", call_order.len()),
            )
        });

        assert_eq!(call_order, vec!["call_1".to_string(), "call_2".to_string()]);

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input.len(), 4);
        assert_eq!(follow_up_input[0]["type"], "function_call");
        assert_eq!(follow_up_input[0]["call_id"], "call_1");
        assert_eq!(follow_up_input[1]["type"], "function_call");
        assert_eq!(follow_up_input[1]["call_id"], "call_2");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(follow_up_input[2]["call_id"], "call_1");
        assert_eq!(follow_up_input[3]["type"], "function_call_output");
        assert_eq!(follow_up_input[3]["call_id"], "call_2");

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_cancels_when_terminal_status_is_cancelled_while_tool_calls_are_pending() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![CodexStreamEvent::ToolCallRequested {
                id: Some("fc_cancel".to_string()),
                call_id: Some("call_cancel".to_string()),
                tool_name: Some("read".to_string()),
                arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
            }],
            terminal: Some(CodexResponseStatus::Cancelled),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not run after cancelled terminal status")
        });

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 9 })
        ));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Cancelled { run_id: 9 })
        ));
        assert_eq!(stream.observed_requests().len(), 1);
    }

    #[test]
    fn run_fails_when_terminal_status_is_non_complete_while_tool_calls_are_pending() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![CodexStreamEvent::ToolCallRequested {
                id: Some("fc_pending".to_string()),
                call_id: Some("call_pending".to_string()),
                tool_name: Some("bash".to_string()),
                arguments: Some(Value::String("{\"command\":\"pwd\"}".to_string())),
            }],
            terminal: Some(CodexResponseStatus::InProgress),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not run for non-complete terminal status")
        });

        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error })
                if error.contains("non-complete terminal status 'in_progress' while processing tool calls")
        ));
        assert_eq!(stream.observed_requests().len(), 1);
    }

    #[test]
    fn run_fails_when_terminal_status_is_missing_while_tool_calls_are_pending() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![CodexStreamEvent::ToolCallRequested {
                id: Some("fc_missing_terminal".to_string()),
                call_id: Some("call_missing_terminal".to_string()),
                tool_name: Some("read".to_string()),
                arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
            }],
            terminal: None,
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not run when terminal status is missing")
        });

        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error })
                if error.contains("without terminal status while processing tool calls")
        ));
        assert_eq!(stream.observed_requests().len(), 1);
    }

    #[test]
    fn run_fails_explicitly_when_tool_call_payload_is_malformed() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![CodexStreamEvent::ToolCallRequested {
                id: Some("fc_1".to_string()),
                call_id: Some("call_1".to_string()),
                tool_name: Some("read".to_string()),
                arguments: Some(Value::String("not-json".to_string())),
            }],
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events_with_executor(&provider, |_call| {
            panic!("malformed tool call payload should fail before tool execution")
        });

        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("arguments must be valid JSON")
        ));
        assert_eq!(stream.observed_requests().len(), 1);
    }

    #[test]
    fn run_fails_explicitly_when_tool_call_is_unsupported() {
        let stream = FakeStreamClient::success(StreamResult {
            events: vec![CodexStreamEvent::ToolCallRequested {
                id: Some("fc_1".to_string()),
                call_id: Some("call_1".to_string()),
                tool_name: Some("apply_patch".to_string()),
                arguments: Some(Value::String("{}".to_string())),
            }],
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let events = run_events_with_executor(&provider, |_call| {
            panic!("unsupported tool call should fail before tool execution")
        });

        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("Unsupported tool call 'apply_patch'")
        ));
        assert_eq!(stream.observed_requests().len(), 1);
    }

    #[test]
    fn run_maps_cancelled_transport_to_cancelled_terminal_event() {
        let stream = FakeStreamClient::failure(CodexApiError::Cancelled);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            stream,
        );

        let events = run_events(&provider);

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 9 })
        ));
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

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 9 })
        ));
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

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 9 })
        ));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("in_progress")
        ));
    }

    #[test]
    fn new_rejects_empty_access_token() {
        let error = init_error(CodexApiProviderConfig::new(
            "   ",
            vec!["gpt-5.1-codex".to_string()],
        ));

        assert!(error.message().contains("non-empty access token"));
    }

    #[test]
    fn new_rejects_empty_model_list() {
        let error = init_error(CodexApiProviderConfig::new("token", Vec::new()));

        assert!(error.message().contains("at least one non-empty model id"));
    }

    #[test]
    fn new_rejects_blank_optional_fields() {
        let error = init_error(
            CodexApiProviderConfig::new("token", vec!["gpt-5.1-codex".to_string()])
                .with_base_url("  "),
        );
        assert!(error.message().contains("base URL"));

        let error = init_error(
            CodexApiProviderConfig::new("token", vec!["gpt-5.1-codex".to_string()])
                .with_session_id("   "),
        );
        assert!(error.message().contains("session id"));
    }

    #[test]
    fn new_rejects_zero_timeout() {
        let error = init_error(
            CodexApiProviderConfig::new("token", vec!["gpt-5.1-codex".to_string()])
                .with_timeout(Duration::from_secs(0)),
        );

        assert!(error.message().contains("greater than zero"));
    }

    #[test]
    fn new_rejects_invalid_base_url() {
        let error = init_error(
            CodexApiProviderConfig::new("token", vec!["gpt-5.1-codex".to_string()])
                .with_base_url("https://exa mple.com"),
        );

        assert!(error.message().contains("base URL is invalid"));
    }
}
