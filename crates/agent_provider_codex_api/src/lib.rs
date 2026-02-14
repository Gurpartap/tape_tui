//! Codex API-backed implementation of the shared `agent_provider` contract.
//!
//! This adapter translates `codex_api` stream semantics into deterministic
//! `RunEvent` lifecycle events expected by `coding_agent`.
//! Initial requests replay full provider-neutral `RunRequest.messages` history into
//! list-shaped Responses `input` items.
//! Host-mediated tool execution is serial and limited to the v1 tool pack
//! (`bash`, `read`, `edit`, `write`, `apply_patch`), with explicit failure/cancel outcomes for
//! malformed payloads or non-complete terminal statuses.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_provider::{
    CancelSignal, ProviderInitError, ProviderProfile, RunEvent, RunMessage, RunProvider,
    RunRequest, ToolCallRequest, ToolDefinition, ToolResult,
};
use codex_api::payload::CodexReasoning;
use codex_api::{
    normalize_codex_url, CodexApiClient, CodexApiConfig, CodexApiError, CodexRequest,
    CodexResponseStatus, CodexStreamEvent, StreamResult,
};
use serde_json::{json, Value};
use url::Url;

/// Stable provider identifier used by `coding_agent` startup selection.
pub const CODEX_API_PROVIDER_ID: &str = "codex-api";

const V1_TOOL_NAMES: [&str; 5] = ["bash", "read", "edit", "write", "apply_patch"];
const THINKING_LEVELS_BASELINE: [&str; 5] = ["off", "minimal", "low", "medium", "high"];
const THINKING_LEVELS_WITH_XHIGH: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];
const SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT: &str = "No result provided";
const NORMALIZED_TOOL_CALL_ID_MAX_LEN: usize = 64;
const NORMALIZED_TOOL_CALL_ID_FALLBACK: &str = "call_0";
const NORMALIZED_TOOL_ITEM_ID_FALLBACK: &str = "fc_0";

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedToolCallId {
    canonical: String,
    transport_call_id: String,
    response_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnresolvedToolCall {
    raw_id: String,
    canonical_id: String,
    transport_call_id: String,
    tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionState {
    model_index: usize,
    thinking_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingToolCall {
    execution_call_id: String,
    replay_call_id: String,
    tool_name: String,
    arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
enum ReplayStepItem {
    AssistantText(String),
    ToolCall(PendingToolCall),
}

#[derive(Debug, Clone, PartialEq)]
struct StreamStepOutcome {
    replay_items: Vec<ReplayStepItem>,
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

    fn stream_with_handler(
        &self,
        request: &CodexRequest,
        cancel: &CancelSignal,
        on_event: &mut dyn FnMut(CodexStreamEvent),
    ) -> Result<Option<CodexResponseStatus>, CodexApiError> {
        let stream_result = self.stream(request, cancel)?;
        for stream_event in stream_result.events {
            on_event(stream_event);
        }

        Ok(stream_result.terminal)
    }
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

    fn stream_with_handler(
        &self,
        request: &CodexRequest,
        cancel: &CancelSignal,
        on_event: &mut dyn FnMut(CodexStreamEvent),
    ) -> Result<Option<CodexResponseStatus>, CodexApiError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                CodexApiError::Unknown(format!("failed to initialize tokio runtime: {error}"))
            })?;

        runtime.block_on(
            self.client
                .stream_with_handler(request, Some(cancel), |event| {
                    on_event(event);
                }),
        )
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
            selection: Mutex::new(SelectionState {
                model_index: 0,
                thinking_index: 0,
            }),
            stream_client,
        })
    }

    fn selected_model_and_thinking(&self) -> (String, String) {
        let selection = lock_unpoisoned(&self.selection);
        let model_id = self.model_ids[selection.model_index].clone();
        let thinking_levels = thinking_levels_for_model(model_id.as_str());
        let thinking_index = selection
            .thinking_index
            .min(thinking_levels.len().saturating_sub(1));

        (model_id, thinking_levels[thinking_index].to_string())
    }

    fn profile_for_selection(&self, selection: &SelectionState) -> ProviderProfile {
        let model_id = self.model_ids[selection.model_index].clone();
        let thinking_levels = thinking_levels_for_model(model_id.as_str());
        let thinking_index = selection
            .thinking_index
            .min(thinking_levels.len().saturating_sub(1));

        ProviderProfile {
            provider_id: CODEX_API_PROVIDER_ID.to_string(),
            model_id,
            thinking_level: Some(thinking_levels[thinking_index].to_string()),
        }
    }

    fn process_stream_event(
        &self,
        run_id: u64,
        stream_event: CodexStreamEvent,
        replay_items: &mut Vec<ReplayStepItem>,
        text_buffer: &mut String,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        match stream_event {
            CodexStreamEvent::OutputTextDelta { delta } => {
                if !delta.is_empty() {
                    text_buffer.push_str(&delta);
                    emit(RunEvent::Chunk {
                        run_id,
                        text: delta,
                    });
                }
            }
            CodexStreamEvent::ReasoningSummaryTextDelta { .. } => {}
            CodexStreamEvent::ToolCallRequested {
                id,
                call_id,
                tool_name,
                arguments,
            } => {
                self.flush_text_buffer(text_buffer, replay_items);
                replay_items.push(ReplayStepItem::ToolCall(parse_pending_tool_call(
                    id, call_id, tool_name, arguments,
                )?));
            }
            _ => {}
        }

        Ok(())
    }

    fn flush_text_buffer(&self, text_buffer: &mut String, replay_items: &mut Vec<ReplayStepItem>) {
        if !text_buffer.is_empty() {
            replay_items.push(ReplayStepItem::AssistantText(std::mem::take(text_buffer)));
        }
    }

    #[cfg(test)]
    fn process_stream_events(
        &self,
        run_id: u64,
        stream_events: Vec<CodexStreamEvent>,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<StreamStepOutcome, String> {
        let mut replay_items = Vec::new();
        let mut text_buffer = String::new();

        for stream_event in stream_events {
            self.process_stream_event(
                run_id,
                stream_event,
                &mut replay_items,
                &mut text_buffer,
                emit,
            )?;
        }

        self.flush_text_buffer(&mut text_buffer, &mut replay_items);

        Ok(StreamStepOutcome { replay_items })
    }

    fn build_initial_request(
        &self,
        model_id: &str,
        thinking_level: &str,
        messages: &[RunMessage],
        instructions: &str,
    ) -> Result<CodexRequest, String> {
        let sanitized_messages = sanitize_run_messages(messages.to_vec())?;
        let normalized_messages = normalize_run_messages_for_codex(sanitized_messages)?;
        let mut request = CodexRequest::new(
            model_id.to_owned(),
            Value::Array(codex_input_from_run_messages(&normalized_messages)?),
            Some(instructions.to_string()),
        );
        request.reasoning = thinking_reasoning_payload(thinking_level);
        request.tools = codex_tool_payloads();
        Ok(request)
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
            selection: Mutex::new(SelectionState {
                model_index: 0,
                thinking_index: 0,
            }),
            stream_client,
        }
    }
}

impl RunProvider for CodexApiProvider {
    fn profile(&self) -> ProviderProfile {
        let selection = lock_unpoisoned(&self.selection);
        self.profile_for_selection(&selection)
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        v1_tool_definitions()
    }

    fn cycle_model(&self) -> Result<ProviderProfile, String> {
        let mut selection = lock_unpoisoned(&self.selection);
        selection.model_index = (selection.model_index + 1) % self.model_ids.len();
        selection.thinking_index = normalize_thinking_index(
            self.model_ids[selection.model_index].as_str(),
            selection.thinking_index,
        );

        Ok(self.profile_for_selection(&selection))
    }

    fn cycle_thinking_level(&self) -> Result<ProviderProfile, String> {
        let mut selection = lock_unpoisoned(&self.selection);
        let thinking_levels =
            thinking_levels_for_model(self.model_ids[selection.model_index].as_str());
        selection.thinking_index = (selection.thinking_index + 1) % thinking_levels.len();

        Ok(self.profile_for_selection(&selection))
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let RunRequest {
            run_id,
            messages,
            instructions,
        } = req;
        let (model_id, thinking_level) = self.selected_model_and_thinking();
        let messages = sanitize_run_messages(messages)?;
        let instructions = sanitize_run_instructions(instructions)?;

        let mut replay_messages = messages;
        let mut request = self.build_initial_request(
            &model_id,
            &thinking_level,
            &replay_messages,
            &instructions,
        )?;

        emit(RunEvent::Started { run_id });

        if cancel.load(Ordering::Acquire) {
            emit(RunEvent::Cancelled { run_id });
            return Ok(());
        }

        loop {
            if cancel.load(Ordering::Acquire) {
                emit(RunEvent::Cancelled { run_id });
                return Ok(());
            }

            let mut replay_items = Vec::new();
            let mut text_buffer = String::new();
            let mut stream_parse_error = None;
            let terminal = match self.stream_client.stream_with_handler(
                &request,
                &cancel,
                &mut |stream_event| {
                    if stream_parse_error.is_some() {
                        return;
                    }

                    if let Err(error) = self.process_stream_event(
                        run_id,
                        stream_event,
                        &mut replay_items,
                        &mut text_buffer,
                        emit,
                    ) {
                        stream_parse_error = Some(error);
                    }
                },
            ) {
                Ok(terminal) => terminal,
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

            if let Some(error) = stream_parse_error {
                emit(RunEvent::Failed { run_id, error });
                return Ok(());
            }

            self.flush_text_buffer(&mut text_buffer, &mut replay_items);
            let stream_outcome = StreamStepOutcome { replay_items };

            let has_pending_tool_calls = stream_outcome
                .replay_items
                .iter()
                .any(|item| matches!(item, ReplayStepItem::ToolCall(_)));

            if !has_pending_tool_calls {
                self.emit_terminal_event(run_id, terminal, emit);
                return Ok(());
            }

            match terminal {
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

            let pending_tool_calls_len = stream_outcome
                .replay_items
                .iter()
                .filter(|item| matches!(item, ReplayStepItem::ToolCall(_)))
                .count();
            let mut pending_tool_calls = Vec::with_capacity(pending_tool_calls_len);

            for replay_item in stream_outcome.replay_items {
                match replay_item {
                    ReplayStepItem::AssistantText(text) => {
                        if !text.is_empty() {
                            replay_messages.push(RunMessage::AssistantText { text });
                        }
                    }
                    ReplayStepItem::ToolCall(pending_call) => {
                        replay_messages.push(RunMessage::ToolCall {
                            call_id: pending_call.replay_call_id.clone(),
                            tool_name: pending_call.tool_name.clone(),
                            arguments: pending_call.arguments.clone(),
                        });
                        pending_tool_calls.push(pending_call);
                    }
                }
            }

            let mut tool_results = Vec::with_capacity(pending_tool_calls.len());
            for pending_call in pending_tool_calls {
                if cancel.load(Ordering::Acquire) {
                    emit(RunEvent::Cancelled { run_id });
                    return Ok(());
                }

                let result = execute_tool(ToolCallRequest {
                    call_id: pending_call.execution_call_id,
                    tool_name: pending_call.tool_name,
                    arguments: pending_call.arguments,
                });

                tool_results.push((pending_call.replay_call_id, result));
            }

            for (replay_call_id, result) in tool_results {
                replay_messages.push(RunMessage::ToolResult {
                    call_id: replay_call_id,
                    tool_name: result.tool_name,
                    content: result.content,
                    is_error: result.is_error,
                });
            }

            request = match self.build_initial_request(
                &model_id,
                &thinking_level,
                &replay_messages,
                &instructions,
            ) {
                Ok(request) => request,
                Err(error) => {
                    emit(RunEvent::Failed { run_id, error });
                    return Ok(());
                }
            };
        }
    }
}

fn thinking_reasoning_payload(thinking_level: &str) -> Option<CodexReasoning> {
    let thinking_level = thinking_level.trim();
    if thinking_level.eq_ignore_ascii_case("off") {
        return None;
    }

    Some(CodexReasoning {
        effort: Some(thinking_level.to_ascii_lowercase()),
        summary: None,
    })
}

fn normalize_thinking_index(model_id: &str, thinking_index: usize) -> usize {
    let thinking_levels = thinking_levels_for_model(model_id);
    thinking_index.min(thinking_levels.len().saturating_sub(1))
}

fn thinking_levels_for_model(model_id: &str) -> &'static [&'static str] {
    if supports_xhigh_thinking(model_id) {
        &THINKING_LEVELS_WITH_XHIGH
    } else {
        &THINKING_LEVELS_BASELINE
    }
}

fn supports_xhigh_thinking(model_id: &str) -> bool {
    let canonical = model_id
        .rsplit('/')
        .next()
        .unwrap_or(model_id)
        .to_ascii_lowercase();

    canonical.contains("codex")
        && (canonical.starts_with("gpt-5.2") || canonical.starts_with("gpt-5.3"))
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
        ToolDefinition {
            name: "apply_patch".to_string(),
            description: Some(
                "Apply an apply_patch-formatted patch to workspace files".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                },
                "required": ["input"],
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

/// Normalization/backfill policy for Codex replay history.
///
/// - Boundary backfill: at every assistant/user boundary, unresolved tool calls without any
///   remaining *raw-id* tool results in the future are backfilled immediately with a synthetic
///   error result (`"No result provided"`).
/// - EOF backfill (current policy): after all messages are processed, any still-unresolved tool
///   calls are always backfilled with the same synthetic error result, preserving unresolved
///   encounter order.
/// - Collision rules: unresolved tool calls must remain unique across both canonical normalized
///   IDs and transport call IDs (`call_id` segment before `|`); either collision hard-fails.
/// - Mapping precedence: tool results first attempt raw-id queue matching against unresolved
///   calls; when no queued raw match exists, normalization fallback is applied to the raw result
///   call ID.
fn normalize_run_messages_for_codex(messages: Vec<RunMessage>) -> Result<Vec<RunMessage>, String> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut unresolved_tool_calls = VecDeque::new();
    let mut unresolved_canonical_ids = HashSet::new();
    let mut unresolved_transport_call_ids = HashSet::new();
    let mut unresolved_canonical_ids_by_raw: HashMap<String, VecDeque<String>> = HashMap::new();
    let mut remaining_tool_result_counts_by_raw =
        build_remaining_tool_result_counts_by_raw(&messages)?;

    for message in messages {
        match message {
            RunMessage::UserText { text } => {
                validate_nonempty_user_text(&text)?;
                flush_unresolved_tool_calls_without_future_results(
                    &mut normalized,
                    &mut unresolved_tool_calls,
                    &mut unresolved_canonical_ids,
                    &mut unresolved_transport_call_ids,
                    &mut unresolved_canonical_ids_by_raw,
                    &remaining_tool_result_counts_by_raw,
                );
                normalized.push(RunMessage::UserText { text });
            }
            RunMessage::AssistantText { text } => {
                validate_nonempty_assistant_text(&text)?;
                flush_unresolved_tool_calls_without_future_results(
                    &mut normalized,
                    &mut unresolved_tool_calls,
                    &mut unresolved_canonical_ids,
                    &mut unresolved_transport_call_ids,
                    &mut unresolved_canonical_ids_by_raw,
                    &remaining_tool_result_counts_by_raw,
                );
                normalized.push(RunMessage::AssistantText { text });
            }
            RunMessage::ToolCall {
                call_id,
                tool_name,
                arguments,
            } => {
                let raw_call_id = sanitize_nonempty_field(&call_id, "tool call call_id")?;
                let normalized_call_id = normalize_tool_call_id_for_codex(raw_call_id.as_str());
                let tool_name = sanitize_nonempty_field(&tool_name, "tool call tool_name")?;
                let _arguments_json = encode_tool_call_arguments(&tool_name, &arguments)?;

                if unresolved_canonical_ids.contains(normalized_call_id.canonical.as_str()) {
                    return Err(duplicate_normalized_unresolved_tool_call_id_error(
                        normalized_call_id.canonical.as_str(),
                    ));
                }

                if unresolved_transport_call_ids
                    .contains(normalized_call_id.transport_call_id.as_str())
                {
                    return Err(duplicate_normalized_unresolved_tool_transport_id_error(
                        normalized_call_id.transport_call_id.as_str(),
                    ));
                }

                unresolved_canonical_ids.insert(normalized_call_id.canonical.clone());
                unresolved_transport_call_ids.insert(normalized_call_id.transport_call_id.clone());
                unresolved_canonical_ids_by_raw
                    .entry(raw_call_id.clone())
                    .or_default()
                    .push_back(normalized_call_id.canonical.clone());
                unresolved_tool_calls.push_back(UnresolvedToolCall {
                    raw_id: raw_call_id,
                    canonical_id: normalized_call_id.canonical.clone(),
                    transport_call_id: normalized_call_id.transport_call_id.clone(),
                    tool_name: tool_name.clone(),
                });
                normalized.push(RunMessage::ToolCall {
                    call_id: normalized_call_id.canonical,
                    tool_name,
                    arguments,
                });
            }
            RunMessage::ToolResult {
                call_id,
                tool_name,
                content,
                is_error,
            } => {
                let raw_call_id = sanitize_nonempty_field(&call_id, "tool result call_id")?;
                let normalized_call_id = pop_unresolved_canonical_id_for_raw(
                    &mut unresolved_canonical_ids_by_raw,
                    raw_call_id.as_str(),
                )
                .unwrap_or_else(|| {
                    normalize_tool_call_id_for_codex(raw_call_id.as_str()).canonical
                });
                let tool_name = sanitize_nonempty_field(&tool_name, "tool result tool_name")?;

                decrement_remaining_tool_result_count(
                    &mut remaining_tool_result_counts_by_raw,
                    raw_call_id.as_str(),
                );

                remove_unresolved_tool_call_by_canonical_id(
                    &mut unresolved_tool_calls,
                    &mut unresolved_canonical_ids,
                    &mut unresolved_transport_call_ids,
                    &mut unresolved_canonical_ids_by_raw,
                    normalized_call_id.as_str(),
                );

                normalized.push(RunMessage::ToolResult {
                    call_id: normalized_call_id,
                    tool_name,
                    content,
                    is_error,
                });
            }
        }
    }

    flush_unresolved_tool_calls(
        &mut normalized,
        &mut unresolved_tool_calls,
        &mut unresolved_canonical_ids,
        &mut unresolved_transport_call_ids,
        &mut unresolved_canonical_ids_by_raw,
    );

    Ok(normalized)
}

fn build_remaining_tool_result_counts_by_raw(
    messages: &[RunMessage],
) -> Result<HashMap<String, usize>, String> {
    let mut remaining_tool_result_counts_by_raw = HashMap::new();

    for message in messages {
        if let RunMessage::ToolResult { call_id, .. } = message {
            let raw_call_id = sanitize_nonempty_field(call_id, "tool result call_id")?;
            *remaining_tool_result_counts_by_raw
                .entry(raw_call_id)
                .or_insert(0) += 1;
        }
    }

    Ok(remaining_tool_result_counts_by_raw)
}

fn flush_unresolved_tool_calls_without_future_results(
    normalized: &mut Vec<RunMessage>,
    unresolved_tool_calls: &mut VecDeque<UnresolvedToolCall>,
    unresolved_canonical_ids: &mut HashSet<String>,
    unresolved_transport_call_ids: &mut HashSet<String>,
    unresolved_canonical_ids_by_raw: &mut HashMap<String, VecDeque<String>>,
    remaining_tool_result_counts_by_raw: &HashMap<String, usize>,
) {
    let mut still_unresolved = VecDeque::new();

    while let Some(unresolved) = unresolved_tool_calls.pop_front() {
        let has_future_result = remaining_tool_result_counts_by_raw
            .get(unresolved.raw_id.as_str())
            .copied()
            .unwrap_or_default()
            > 0;

        if has_future_result {
            still_unresolved.push_back(unresolved);
        } else {
            normalized.push(RunMessage::ToolResult {
                call_id: unresolved.canonical_id,
                tool_name: unresolved.tool_name,
                content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                is_error: true,
            });
        }
    }

    *unresolved_tool_calls = still_unresolved;
    rebuild_unresolved_indexes(
        unresolved_tool_calls,
        unresolved_canonical_ids,
        unresolved_transport_call_ids,
        unresolved_canonical_ids_by_raw,
    );
}

fn pop_unresolved_canonical_id_for_raw(
    unresolved_canonical_ids_by_raw: &mut HashMap<String, VecDeque<String>>,
    raw_call_id: &str,
) -> Option<String> {
    let mut should_remove = false;
    let canonical_id = unresolved_canonical_ids_by_raw
        .get_mut(raw_call_id)
        .and_then(|canonical_ids| {
            let canonical_id = canonical_ids.pop_front();
            should_remove = canonical_ids.is_empty();
            canonical_id
        });

    if should_remove {
        unresolved_canonical_ids_by_raw.remove(raw_call_id);
    }

    canonical_id
}

fn remove_unresolved_tool_call_by_canonical_id(
    unresolved_tool_calls: &mut VecDeque<UnresolvedToolCall>,
    unresolved_canonical_ids: &mut HashSet<String>,
    unresolved_transport_call_ids: &mut HashSet<String>,
    unresolved_canonical_ids_by_raw: &mut HashMap<String, VecDeque<String>>,
    canonical_id: &str,
) {
    let Some(position) = unresolved_tool_calls
        .iter()
        .position(|unresolved| unresolved.canonical_id == canonical_id)
    else {
        return;
    };

    let removed = unresolved_tool_calls
        .remove(position)
        .expect("position returned from VecDeque::position must be valid");
    unresolved_canonical_ids.remove(canonical_id);
    unresolved_transport_call_ids.remove(removed.transport_call_id.as_str());

    if let Some(canonical_ids) = unresolved_canonical_ids_by_raw.get_mut(removed.raw_id.as_str()) {
        if let Some(index) = canonical_ids
            .iter()
            .position(|queued_canonical_id| queued_canonical_id == canonical_id)
        {
            canonical_ids.remove(index);
        }

        if canonical_ids.is_empty() {
            unresolved_canonical_ids_by_raw.remove(removed.raw_id.as_str());
        }
    }
}

fn rebuild_unresolved_indexes(
    unresolved_tool_calls: &VecDeque<UnresolvedToolCall>,
    unresolved_canonical_ids: &mut HashSet<String>,
    unresolved_transport_call_ids: &mut HashSet<String>,
    unresolved_canonical_ids_by_raw: &mut HashMap<String, VecDeque<String>>,
) {
    unresolved_canonical_ids.clear();
    unresolved_transport_call_ids.clear();
    unresolved_canonical_ids_by_raw.clear();

    for unresolved in unresolved_tool_calls {
        unresolved_canonical_ids.insert(unresolved.canonical_id.clone());
        unresolved_transport_call_ids.insert(unresolved.transport_call_id.clone());
        unresolved_canonical_ids_by_raw
            .entry(unresolved.raw_id.clone())
            .or_default()
            .push_back(unresolved.canonical_id.clone());
    }
}

fn decrement_remaining_tool_result_count(
    remaining_tool_result_counts_by_raw: &mut HashMap<String, usize>,
    raw_call_id: &str,
) {
    let mut should_remove = false;

    if let Some(count) = remaining_tool_result_counts_by_raw.get_mut(raw_call_id) {
        *count = count.saturating_sub(1);
        should_remove = *count == 0;
    }

    if should_remove {
        remaining_tool_result_counts_by_raw.remove(raw_call_id);
    }
}

fn normalize_tool_call_id_for_codex(raw: &str) -> NormalizedToolCallId {
    let trimmed = raw.trim();

    let (call_raw, item_raw) = match trimmed.split_once('|') {
        Some((call_part, item_part)) => (call_part, Some(item_part)),
        None => (trimmed, None),
    };

    let call_segment =
        normalize_tool_call_id_segment(call_raw.trim(), NORMALIZED_TOOL_CALL_ID_FALLBACK);

    let response_item_id = item_raw.map(|item_part| {
        let normalized_item =
            normalize_tool_call_id_segment(item_part.trim(), NORMALIZED_TOOL_ITEM_ID_FALLBACK);
        if normalized_item.starts_with("fc") {
            normalized_item
        } else {
            format!("fc_{normalized_item}")
        }
    });

    let canonical = match response_item_id.as_deref() {
        Some(item_id) => format!("{call_segment}|{item_id}"),
        None => call_segment.clone(),
    };

    NormalizedToolCallId {
        canonical,
        transport_call_id: call_segment,
        response_item_id,
    }
}

fn normalize_tool_call_id_segment(raw_segment: &str, fallback: &str) -> String {
    let mut normalized =
        String::with_capacity(raw_segment.len().min(NORMALIZED_TOOL_CALL_ID_MAX_LEN));

    for character in raw_segment.chars() {
        let mapped = if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            character
        } else {
            '_'
        };
        normalized.push(mapped);

        if normalized.len() >= NORMALIZED_TOOL_CALL_ID_MAX_LEN {
            break;
        }
    }

    while normalized.ends_with('_') {
        normalized.pop();
    }

    if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized
    }
}

fn flush_unresolved_tool_calls(
    normalized: &mut Vec<RunMessage>,
    unresolved_tool_calls: &mut VecDeque<UnresolvedToolCall>,
    unresolved_canonical_ids: &mut HashSet<String>,
    unresolved_transport_call_ids: &mut HashSet<String>,
    unresolved_canonical_ids_by_raw: &mut HashMap<String, VecDeque<String>>,
) {
    while let Some(unresolved) = unresolved_tool_calls.pop_front() {
        normalized.push(RunMessage::ToolResult {
            call_id: unresolved.canonical_id,
            tool_name: unresolved.tool_name,
            content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
            is_error: true,
        });
    }

    unresolved_canonical_ids.clear();
    unresolved_transport_call_ids.clear();
    unresolved_canonical_ids_by_raw.clear();
}

fn duplicate_normalized_unresolved_tool_call_id_error(call_id: &str) -> String {
    format!(
        "codex-api provider cannot normalize run history: duplicate normalized unresolved tool call id '{call_id}'"
    )
}

fn duplicate_normalized_unresolved_tool_transport_id_error(call_id: &str) -> String {
    format!(
        "codex-api provider cannot normalize run history: duplicate normalized unresolved tool transport id '{call_id}'"
    )
}

fn split_canonical_tool_call_id(canonical_call_id: &str) -> (&str, Option<&str>) {
    match canonical_call_id.split_once('|') {
        Some((transport_call_id, "")) => (transport_call_id, None),
        Some((transport_call_id, response_item_id)) => (transport_call_id, Some(response_item_id)),
        None => (canonical_call_id, None),
    }
}

fn codex_input_from_run_messages(messages: &[RunMessage]) -> Result<Vec<Value>, String> {
    let mut input = Vec::with_capacity(messages.len());
    let mut assistant_message_index = 0usize;

    for message in messages {
        match message {
            RunMessage::UserText { text } => {
                input.push(codex_user_text_message(text)?);
            }
            RunMessage::AssistantText { text } => {
                input.push(codex_assistant_output_message(
                    text,
                    assistant_message_index,
                )?);
                assistant_message_index += 1;
            }
            RunMessage::ToolCall {
                call_id,
                tool_name,
                arguments,
            } => {
                let canonical_call_id = sanitize_nonempty_field(call_id, "tool call call_id")?;
                let (transport_call_id, response_item_id) =
                    split_canonical_tool_call_id(canonical_call_id.as_str());
                let tool_name = sanitize_nonempty_field(tool_name, "tool call tool_name")?;
                let arguments_json = encode_tool_call_arguments(tool_name.as_str(), arguments)?;
                let mut function_call = json!({
                    "type": "function_call",
                    "call_id": transport_call_id,
                    "name": tool_name,
                    "arguments": arguments_json,
                });
                if let Some(response_item_id) = response_item_id {
                    function_call["id"] = Value::String(response_item_id.to_string());
                }
                input.push(function_call);
            }
            RunMessage::ToolResult {
                call_id,
                tool_name,
                content,
                ..
            } => {
                let canonical_call_id = sanitize_nonempty_field(call_id, "tool result call_id")?;
                let (transport_call_id, _) =
                    split_canonical_tool_call_id(canonical_call_id.as_str());
                let _tool_name = sanitize_nonempty_field(tool_name, "tool result tool_name")?;
                let output = Value::String(tool_result_content_text(content));
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": transport_call_id,
                    "output": output,
                }));
            }
        }
    }

    Ok(input)
}

fn validate_nonempty_user_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(
            "codex-api provider requires non-empty user text messages in run history".to_string(),
        );
    }

    Ok(())
}

fn validate_nonempty_assistant_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(
            "codex-api provider requires non-empty assistant text messages in run history"
                .to_string(),
        );
    }

    Ok(())
}

fn codex_user_text_message(text: &str) -> Result<Value, String> {
    validate_nonempty_user_text(text)?;

    Ok(json!({
        "role": "user",
        "content": [
            {
                "type": "input_text",
                "text": text,
            }
        ],
    }))
}

fn codex_assistant_output_message(text: &str, message_index: usize) -> Result<Value, String> {
    validate_nonempty_assistant_text(text)?;

    Ok(json!({
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "output_text",
                "text": text,
                "annotations": [],
            }
        ],
        "status": "completed",
        "id": format!("msg_{message_index}"),
    }))
}

fn encode_tool_call_arguments(tool_name: &str, arguments: &Value) -> Result<String, String> {
    if !arguments.is_object() {
        return Err(format!(
            "codex-api provider requires tool call arguments to be a JSON object for tool '{tool_name}'"
        ));
    }

    serde_json::to_string(arguments).map_err(|error| {
        format!(
            "codex-api provider failed to serialize tool call arguments for '{tool_name}': {error}"
        )
    })
}

fn sanitize_nonempty_field(value: &str, field_name: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "codex-api provider requires non-empty {field_name} in run history"
        ));
    }

    Ok(trimmed.to_string())
}

fn parse_pending_tool_call(
    id: Option<String>,
    call_id: Option<String>,
    tool_name: Option<String>,
    arguments: Option<Value>,
) -> Result<PendingToolCall, String> {
    let execution_call_id = required_stream_string(call_id, "call_id")?;
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

    let arguments = normalize_tool_arguments(&tool_name, arguments)?;
    let replay_raw_call_id = match sanitize_optional_stream_string(id) {
        Some(item_id) => format!("{}|{item_id}", execution_call_id),
        None => execution_call_id.clone(),
    };
    let replay_call_id = normalize_tool_call_id_for_codex(replay_raw_call_id.as_str()).canonical;

    Ok(PendingToolCall {
        execution_call_id,
        replay_call_id,
        tool_name,
        arguments,
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

fn sanitize_optional_stream_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_tool_arguments(tool_name: &str, arguments: Value) -> Result<Value, String> {
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

            Ok(parsed)
        }
        Value::Object(_) => Ok(arguments),
        other => Err(format!(
            "Malformed tool call payload for '{tool_name}': arguments must be a JSON object or string, got {}",
            value_type_name(&other)
        )),
    }
}

fn tool_result_content_text(value: &Value) -> String {
    match value {
        Value::String(content) => content.clone(),
        other => other.to_string(),
    }
}

fn sanitize_run_messages(messages: Vec<RunMessage>) -> Result<Vec<RunMessage>, String> {
    if messages.is_empty() {
        return Err(
            "codex-api provider requires non-empty run message history before sending requests"
                .to_string(),
        );
    }

    let has_user_message = messages
        .iter()
        .any(|message| matches!(message, RunMessage::UserText { .. }));
    if !has_user_message {
        return Err(
            "codex-api provider requires at least one user text message in run history".to_string(),
        );
    }

    Ok(messages)
}

fn sanitize_run_instructions(instructions: String) -> Result<String, String> {
    let trimmed = instructions.trim();
    if trimmed.is_empty() {
        return Err(
            "codex-api provider requires non-empty run instructions before sending requests"
                .to_string(),
        );
    }

    Ok(trimmed.to_string())
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};

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

    struct FakeIncrementalStreamClient {
        observed_requests: Mutex<Vec<CodexRequest>>,
        chunk_processed: Arc<AtomicBool>,
        callback_invocations: AtomicUsize,
    }

    impl FakeIncrementalStreamClient {
        fn with_chunk_processed_flag(chunk_processed: Arc<AtomicBool>) -> Arc<Self> {
            Arc::new(Self {
                observed_requests: Mutex::new(Vec::new()),
                chunk_processed,
                callback_invocations: AtomicUsize::new(0),
            })
        }

        fn observed_requests(&self) -> Vec<CodexRequest> {
            lock_unpoisoned(&self.observed_requests).clone()
        }

        fn callback_invocations(&self) -> usize {
            self.callback_invocations.load(Ordering::Acquire)
        }
    }

    impl StreamClient for FakeIncrementalStreamClient {
        fn stream(
            &self,
            _request: &CodexRequest,
            _cancel: &CancelSignal,
        ) -> Result<StreamResult, CodexApiError> {
            panic!("incremental stream client should use stream_with_handler")
        }

        fn stream_with_handler(
            &self,
            request: &CodexRequest,
            _cancel: &CancelSignal,
            on_event: &mut dyn FnMut(CodexStreamEvent),
        ) -> Result<Option<CodexResponseStatus>, CodexApiError> {
            lock_unpoisoned(&self.observed_requests).push(request.clone());

            on_event(CodexStreamEvent::OutputTextDelta {
                delta: "Hello".to_string(),
            });
            self.callback_invocations.fetch_add(1, Ordering::AcqRel);

            if !self.chunk_processed.load(Ordering::Acquire) {
                return Err(CodexApiError::Unknown(
                    "chunk callback was not processed incrementally".to_string(),
                ));
            }

            on_event(CodexStreamEvent::ResponseCompleted {
                status: Some(CodexResponseStatus::Completed),
            });
            self.callback_invocations.fetch_add(1, Ordering::AcqRel);

            Ok(Some(CodexResponseStatus::Completed))
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
                    messages: vec![RunMessage::UserText {
                        text: "hello".to_string(),
                    }],
                    instructions: "system instructions".to_string(),
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

    fn assert_transport_invariants(request: &CodexRequest, instructions: &str) {
        assert_eq!(request.instructions.as_deref(), Some(instructions));
        assert!(
            request.input.is_array(),
            "request input must be list-shaped"
        );
        assert!(!request.store, "request must force store=false");
        assert!(request.stream, "request must force stream=true");
        assert_eq!(
            request.include,
            vec!["reasoning.encrypted_content".to_string()]
        );
        assert_eq!(request.tool_choice.as_deref(), Some("auto"));
        assert!(
            request.parallel_tool_calls,
            "request must force parallel_tool_calls=true"
        );
    }

    #[test]
    fn normalize_run_messages_backfills_orphan_tool_call_before_assistant_boundary() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::AssistantText {
                    text: "turn-1 assistant".to_string(),
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_backfills_orphan_tool_call_before_user_boundary() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::UserText {
                text: "turn-2 user".to_string(),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::UserText {
                    text: "turn-2 user".to_string(),
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_backfills_orphan_tool_call_at_end_of_history() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_preserves_real_tool_result_without_synthetic_backfill() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolResult {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                content: json!("file contents"),
                is_error: false,
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(normalized.len(), 4);
        assert_eq!(
            normalized[2],
            RunMessage::ToolResult {
                call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                content: json!("file contents"),
                is_error: false,
            }
        );
    }

    #[test]
    fn normalize_run_messages_normalizes_tool_call_ids_and_maps_tool_results() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: " call id! ".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolResult {
                call_id: "call id!".to_string(),
                tool_name: "read".to_string(),
                content: json!("file contents"),
                is_error: false,
            },
            RunMessage::ToolCall {
                call_id: "!!!".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
            RunMessage::ToolResult {
                call_id: "!!!".to_string(),
                tool_name: "write".to_string(),
                content: json!("ok"),
                is_error: false,
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call_id".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_id".to_string(),
                    tool_name: "read".to_string(),
                    content: json!("file contents"),
                    is_error: false,
                },
                RunMessage::ToolCall {
                    call_id: "call_0".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_0".to_string(),
                    tool_name: "write".to_string(),
                    content: json!("ok"),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_tool_result_remaps_to_canonical_id_via_raw_id_queue() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: " call id! | item id! ".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolResult {
                call_id: "call id! | item id!".to_string(),
                tool_name: "read".to_string(),
                content: json!("file contents"),
                is_error: false,
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call_id|fc_item_id".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call_id|fc_item_id".to_string(),
                    tool_name: "read".to_string(),
                    content: json!("file contents"),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_repeated_same_raw_call_id_across_turns_works() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "repeat!".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolResult {
                call_id: "repeat!".to_string(),
                tool_name: "read".to_string(),
                content: json!("first"),
                is_error: false,
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
            RunMessage::UserText {
                text: "turn-2 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "repeat!".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
            RunMessage::ToolResult {
                call_id: "repeat!".to_string(),
                tool_name: "write".to_string(),
                content: json!("second"),
                is_error: false,
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "repeat".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "repeat".to_string(),
                    tool_name: "read".to_string(),
                    content: json!("first"),
                    is_error: false,
                },
                RunMessage::AssistantText {
                    text: "turn-1 assistant".to_string(),
                },
                RunMessage::UserText {
                    text: "turn-2 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "repeat".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                },
                RunMessage::ToolResult {
                    call_id: "repeat".to_string(),
                    tool_name: "write".to_string(),
                    content: json!("second"),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_duplicate_normalized_unresolved_call_id_is_hard_fail() {
        let result = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call 1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolCall {
                call_id: "call@1".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ]);

        let error = result.expect_err("normalization collision should hard-fail");
        assert!(
            error.contains("duplicate normalized unresolved tool call id"),
            "error must report normalized unresolved id collisions"
        );
    }

    #[test]
    fn normalize_run_messages_normalization_collision_error_string_is_exact() {
        let result = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call 1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolCall {
                call_id: "call@1".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ]);

        let error = result.expect_err("normalization collision should hard-fail");
        assert_eq!(
            error,
            "codex-api provider cannot normalize run history: duplicate normalized unresolved tool call id 'call_1'"
        );
    }

    #[test]
    fn normalize_tool_call_id_for_codex_sanitizes_simple_id() {
        let normalized = normalize_tool_call_id_for_codex(" call id! ");

        assert_eq!(
            normalized,
            NormalizedToolCallId {
                canonical: "call_id".to_string(),
                transport_call_id: "call_id".to_string(),
                response_item_id: None,
            }
        );
    }

    #[test]
    fn normalize_tool_call_id_for_codex_sanitizes_pipe_segments() {
        let normalized = normalize_tool_call_id_for_codex(" call id! | item id! ");

        assert_eq!(
            normalized,
            NormalizedToolCallId {
                canonical: "call_id|fc_item_id".to_string(),
                transport_call_id: "call_id".to_string(),
                response_item_id: Some("fc_item_id".to_string()),
            }
        );
    }

    #[test]
    fn normalize_tool_call_id_for_codex_enforces_fc_prefix_on_item_segment() {
        let normalized = normalize_tool_call_id_for_codex("call_1|item_1");

        assert_eq!(
            normalized,
            NormalizedToolCallId {
                canonical: "call_1|fc_item_1".to_string(),
                transport_call_id: "call_1".to_string(),
                response_item_id: Some("fc_item_1".to_string()),
            }
        );

        let already_prefixed = normalize_tool_call_id_for_codex("call_1|fc_item_2");
        assert_eq!(
            already_prefixed,
            NormalizedToolCallId {
                canonical: "call_1|fc_item_2".to_string(),
                transport_call_id: "call_1".to_string(),
                response_item_id: Some("fc_item_2".to_string()),
            }
        );
    }

    #[test]
    fn normalize_tool_call_id_for_codex_truncates_each_segment_to_64() {
        let call_segment = "a".repeat(80);
        let item_segment = "b".repeat(80);
        let raw = format!("{call_segment}|{item_segment}");

        let normalized = normalize_tool_call_id_for_codex(raw.as_str());

        assert_eq!(normalized.transport_call_id, "a".repeat(64));
        assert_eq!(
            normalized.response_item_id,
            Some(format!("fc_{}", "b".repeat(64)))
        );
        assert_eq!(
            normalized.canonical,
            format!("{}|fc_{}", "a".repeat(64), "b".repeat(64))
        );
    }

    #[test]
    fn normalize_tool_call_id_for_codex_applies_fallbacks_when_segments_empty() {
        let normalized = normalize_tool_call_id_for_codex("  |   ");

        assert_eq!(
            normalized,
            NormalizedToolCallId {
                canonical: "call_0|fc_0".to_string(),
                transport_call_id: "call_0".to_string(),
                response_item_id: Some("fc_0".to_string()),
            }
        );
    }

    #[test]
    fn normalize_run_messages_duplicate_unresolved_transport_call_id_is_hard_fail() {
        let result = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call|fc_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolCall {
                call_id: "call|fc_2".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ]);

        let error = result.expect_err("transport collision should hard-fail");
        assert!(
            error.contains("duplicate normalized unresolved tool transport id"),
            "error must report unresolved transport id collisions"
        );
    }

    #[test]
    fn normalize_run_messages_duplicate_unresolved_transport_call_id_error_string_is_exact() {
        let result = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call|fc_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolCall {
                call_id: "call|fc_2".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ]);

        let error = result.expect_err("transport collision should hard-fail");
        assert_eq!(
            error,
            "codex-api provider cannot normalize run history: duplicate normalized unresolved tool transport id 'call'"
        );
    }

    #[test]
    fn normalize_run_messages_raw_id_future_result_prevents_false_match_across_normalization_collision(
    ) {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call!".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call@".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
            RunMessage::ToolResult {
                call_id: "call@".to_string(),
                tool_name: "write".to_string(),
                content: json!("ok"),
                is_error: false,
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::AssistantText {
                    text: "turn-1 assistant".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                },
                RunMessage::ToolResult {
                    call_id: "call".to_string(),
                    tool_name: "write".to_string(),
                    content: json!("ok"),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_boundary_backfill_uses_raw_id_not_normalized_id() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call!".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
            RunMessage::ToolResult {
                call_id: "call@".to_string(),
                tool_name: "write".to_string(),
                content: json!("unmatched output"),
                is_error: false,
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::AssistantText {
                    text: "turn-1 assistant".to_string(),
                },
                RunMessage::ToolResult {
                    call_id: "call".to_string(),
                    tool_name: "write".to_string(),
                    content: json!("unmatched output"),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_eof_backfill_remains_deterministic() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "z!".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::ToolCall {
                call_id: "a!".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "z".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolCall {
                    call_id: "a".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                },
                RunMessage::ToolResult {
                    call_id: "z".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::ToolResult {
                    call_id: "a".to_string(),
                    tool_name: "write".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn normalize_run_messages_backfill_policy_is_boundary_then_eof() {
        let normalized = normalize_run_messages_for_codex(vec![
            RunMessage::UserText {
                text: "turn-1 user".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call-boundary!".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            },
            RunMessage::AssistantText {
                text: "turn-1 assistant".to_string(),
            },
            RunMessage::ToolCall {
                call_id: "call-eof!".to_string(),
                tool_name: "write".to_string(),
                arguments: json!({ "path": "README.md", "content": "updated" }),
            },
        ])
        .expect("normalization should succeed");

        assert_eq!(
            normalized,
            vec![
                RunMessage::UserText {
                    text: "turn-1 user".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call-boundary".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call-boundary".to_string(),
                    tool_name: "read".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
                RunMessage::AssistantText {
                    text: "turn-1 assistant".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call-eof".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                },
                RunMessage::ToolResult {
                    call_id: "call-eof".to_string(),
                    tool_name: "write".to_string(),
                    content: Value::String(SYNTHETIC_ORPHAN_TOOL_RESULT_CONTENT.to_string()),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn run_initial_request_replays_synthetic_orphan_tool_result_after_normalization() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 9,
                    messages: vec![
                        RunMessage::UserText {
                            text: "turn-1 user".to_string(),
                        },
                        RunMessage::ToolCall {
                            call_id: "call_1".to_string(),
                            tool_name: "read".to_string(),
                            arguments: json!({ "path": "README.md" }),
                        },
                        RunMessage::AssistantText {
                            text: "turn-1 assistant".to_string(),
                        },
                    ],
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |_call| ToolResult::error("unused", "unused", "unused"),
                &mut |event| events.push(event),
            )
            .expect("run should succeed");

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        let initial_input = requests[0]
            .input
            .as_array()
            .expect("initial request input should be an array");
        assert_eq!(initial_input.len(), 4);
        assert_eq!(initial_input[0]["role"], "user");
        assert_eq!(initial_input[1]["type"], "function_call");
        assert_eq!(initial_input[1]["call_id"], "call_1");
        assert_eq!(initial_input[2]["type"], "function_call_output");
        assert_eq!(initial_input[2]["call_id"], "call_1");
        assert_eq!(initial_input[2]["output"], "No result provided");
        assert_eq!(initial_input[3]["type"], "message");
        assert_eq!(initial_input[3]["role"], "assistant");

        assert!(matches!(
            events.as_slice(),
            [
                RunEvent::Started { run_id: 9 },
                RunEvent::Finished { run_id: 9 }
            ]
        ));
    }

    #[test]
    fn codex_input_from_run_messages_splits_canonical_pipe_id_for_function_call() {
        let input = codex_input_from_run_messages(&[RunMessage::ToolCall {
            call_id: "call_1|fc_item_1".to_string(),
            tool_name: "read".to_string(),
            arguments: json!({ "path": "README.md" }),
        }])
        .expect("conversion should succeed");

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "call_1");
        assert_eq!(input[0]["id"], "fc_item_1");
    }

    #[test]
    fn codex_input_from_run_messages_uses_transport_call_id_for_function_call_output() {
        let input = codex_input_from_run_messages(&[RunMessage::ToolResult {
            call_id: "call_1|fc_item_1".to_string(),
            tool_name: "read".to_string(),
            content: json!("file contents"),
            is_error: false,
        }])
        .expect("conversion should succeed");

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_1");
        assert!(input[0].get("id").is_none());
    }

    #[test]
    fn run_request_payload_normalizes_and_splits_call_ids_in_outbound_payload() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 9,
                    messages: vec![
                        RunMessage::UserText {
                            text: "turn-1 user".to_string(),
                        },
                        RunMessage::ToolCall {
                            call_id: " call id! | item id! ".to_string(),
                            tool_name: "read".to_string(),
                            arguments: json!({ "path": "README.md" }),
                        },
                        RunMessage::ToolResult {
                            call_id: "call id! | item id!".to_string(),
                            tool_name: "read".to_string(),
                            content: json!("file contents"),
                            is_error: false,
                        },
                    ],
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |_call| ToolResult::error("unused", "unused", "unused"),
                &mut |event| events.push(event),
            )
            .expect("run should succeed");

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        let initial_input = requests[0]
            .input
            .as_array()
            .expect("initial request input should be an array");
        assert_eq!(initial_input[1]["type"], "function_call");
        assert_eq!(initial_input[1]["call_id"], "call_id");
        assert_eq!(initial_input[1]["id"], "fc_item_id");
        assert_eq!(initial_input[2]["type"], "function_call_output");
        assert_eq!(initial_input[2]["call_id"], "call_id");
        assert!(initial_input[2].get("id").is_none());

        assert!(matches!(
            events.as_slice(),
            [
                RunEvent::Started { run_id: 9 },
                RunEvent::Finished { run_id: 9 }
            ]
        ));
    }

    #[test]
    fn run_fails_before_http_on_duplicate_normalized_unresolved_tool_call_id_with_exact_error() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 9,
                messages: vec![
                    RunMessage::UserText {
                        text: "turn-1 user".to_string(),
                    },
                    RunMessage::ToolCall {
                        call_id: "call_1".to_string(),
                        tool_name: "read".to_string(),
                        arguments: json!({ "path": "README.md" }),
                    },
                    RunMessage::ToolCall {
                        call_id: "call_1".to_string(),
                        tool_name: "write".to_string(),
                        arguments: json!({ "path": "README.md", "content": "file contents" }),
                    },
                ],
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("duplicate unresolved call ids should fail fast");
        assert_eq!(
            error,
            "codex-api provider cannot normalize run history: duplicate normalized unresolved tool call id 'call_1'"
        );
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
    }

    #[test]
    fn profile_reports_codex_provider_id_selected_model_and_thinking_level() {
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
        assert_eq!(initial.thinking_level.as_deref(), Some("off"));

        let switched = provider
            .cycle_model()
            .expect("codex provider should support model cycling");
        assert_eq!(switched.model_id, "gpt-5.2-codex");
        assert_eq!(switched.thinking_level.as_deref(), Some("off"));
    }

    #[test]
    fn thinking_cycle_order_is_deterministic_and_model_family_aware() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string(), "gpt-5.3-codex".to_string()],
            stream,
        );

        let mut first_family = Vec::new();
        for _ in 0..5 {
            let profile = provider
                .cycle_thinking_level()
                .expect("thinking cycling should be supported");
            first_family.push(
                profile
                    .thinking_level
                    .expect("profile should include thinking level"),
            );
        }
        assert_eq!(
            first_family,
            vec!["minimal", "low", "medium", "high", "off"]
        );

        let switched = provider
            .cycle_model()
            .expect("model cycling should be supported");
        assert_eq!(switched.model_id, "gpt-5.3-codex");
        assert_eq!(switched.thinking_level.as_deref(), Some("off"));

        let mut xhigh_family = Vec::new();
        for _ in 0..6 {
            let profile = provider
                .cycle_thinking_level()
                .expect("thinking cycling should be supported");
            xhigh_family.push(
                profile
                    .thinking_level
                    .expect("profile should include thinking level"),
            );
        }
        assert_eq!(
            xhigh_family,
            vec!["minimal", "low", "medium", "high", "xhigh", "off"]
        );
    }

    #[test]
    fn model_cycle_clamps_xhigh_to_high_when_next_model_does_not_support_it() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.3-codex".to_string(), "gpt-5.1-codex".to_string()],
            stream,
        );

        for _ in 0..5 {
            provider
                .cycle_thinking_level()
                .expect("thinking cycling should be supported");
        }
        assert_eq!(provider.profile().thinking_level.as_deref(), Some("xhigh"));

        let switched = provider
            .cycle_model()
            .expect("model cycling should be supported");
        assert_eq!(switched.model_id, "gpt-5.1-codex");
        assert_eq!(switched.thinking_level.as_deref(), Some("high"));
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
            requests[0].instructions.as_deref(),
            Some("system instructions")
        );
        let initial_input = requests[0]
            .input
            .as_array()
            .expect("initial request input should be an array");
        assert_eq!(initial_input.len(), 1);
        assert_eq!(initial_input[0]["role"], "user");
        assert_eq!(initial_input[0]["content"][0]["type"], "input_text");
        assert_eq!(initial_input[0]["content"][0]["text"], "hello");
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
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RunEvent::Chunk { text, .. } if text == " world")),
            "reasoning-summary deltas must not be emitted as assistant output chunks"
        );
        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_uses_incremental_stream_handler_and_emits_chunk_before_terminal() {
        let chunk_processed = Arc::new(AtomicBool::new(false));
        let stream =
            FakeIncrementalStreamClient::with_chunk_processed_flag(Arc::clone(&chunk_processed));
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 21,
                    messages: vec![RunMessage::UserText {
                        text: "hello".to_string(),
                    }],
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |_call| {
                    panic!("tool executor should not be invoked when no tool calls are streamed")
                },
                &mut |event| {
                    if matches!(event, RunEvent::Chunk { .. }) {
                        chunk_processed.store(true, Ordering::Release);
                    }
                    events.push(event);
                },
            )
            .expect("run should succeed");

        assert!(chunk_processed.load(Ordering::Acquire));
        assert_eq!(stream.callback_invocations(), 2);
        assert_eq!(stream.observed_requests().len(), 1);
        assert_eq!(
            events,
            vec![
                RunEvent::Started { run_id: 21 },
                RunEvent::Chunk {
                    run_id: 21,
                    text: "Hello".to_string(),
                },
                RunEvent::Finished { run_id: 21 },
            ]
        );
    }

    #[test]
    fn process_stream_events_flushes_text_buffer_around_tool_calls() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut emitted = Vec::new();
        let outcome = provider
            .process_stream_events(
                42,
                vec![
                    CodexStreamEvent::OutputTextDelta {
                        delta: "A".to_string(),
                    },
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_1".to_string()),
                        call_id: Some("call_1".to_string()),
                        tool_name: Some("read".to_string()),
                        arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
                    },
                    CodexStreamEvent::OutputTextDelta {
                        delta: "B".to_string(),
                    },
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_2".to_string()),
                        call_id: Some("call_2".to_string()),
                        tool_name: Some("write".to_string()),
                        arguments: Some(Value::String(
                            "{\"path\":\"README.md\",\"content\":\"updated\"}".to_string(),
                        )),
                    },
                    CodexStreamEvent::OutputTextDelta {
                        delta: "C".to_string(),
                    },
                ],
                &mut |event| emitted.push(event),
            )
            .expect("stream events should normalize");

        assert_eq!(
            emitted,
            vec![
                RunEvent::Chunk {
                    run_id: 42,
                    text: "A".to_string(),
                },
                RunEvent::Chunk {
                    run_id: 42,
                    text: "B".to_string(),
                },
                RunEvent::Chunk {
                    run_id: 42,
                    text: "C".to_string(),
                },
            ]
        );
        assert_eq!(
            outcome.replay_items,
            vec![
                ReplayStepItem::AssistantText("A".to_string()),
                ReplayStepItem::ToolCall(PendingToolCall {
                    execution_call_id: "call_1".to_string(),
                    replay_call_id: "call_1|fc_1".to_string(),
                    tool_name: "read".to_string(),
                    arguments: json!({ "path": "README.md" }),
                }),
                ReplayStepItem::AssistantText("B".to_string()),
                ReplayStepItem::ToolCall(PendingToolCall {
                    execution_call_id: "call_2".to_string(),
                    replay_call_id: "call_2|fc_2".to_string(),
                    tool_name: "write".to_string(),
                    arguments: json!({ "path": "README.md", "content": "updated" }),
                }),
                ReplayStepItem::AssistantText("C".to_string()),
            ]
        );
    }

    #[test]
    fn process_stream_events_preserves_stream_item_id_in_replay_call_id_when_present() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut emitted = Vec::new();
        let outcome = provider
            .process_stream_events(
                42,
                vec![CodexStreamEvent::ToolCallRequested {
                    id: Some("item_1".to_string()),
                    call_id: Some("call_1".to_string()),
                    tool_name: Some("read".to_string()),
                    arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
                }],
                &mut |event| emitted.push(event),
            )
            .expect("stream events should normalize");

        assert!(emitted.is_empty());
        assert_eq!(
            outcome.replay_items,
            vec![ReplayStepItem::ToolCall(PendingToolCall {
                execution_call_id: "call_1".to_string(),
                replay_call_id: "call_1|fc_item_1".to_string(),
                tool_name: "read".to_string(),
                arguments: json!({ "path": "README.md" }),
            })]
        );
    }

    #[test]
    fn run_initial_request_uses_list_shaped_input_payload() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let _events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not be called when no tool calls are emitted")
        });

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].input.is_array(),
            "initial codex request input must be a list/array"
        );
        assert!(
            !requests[0].input.is_string(),
            "initial codex request input must never be a plain string"
        );
        assert!(
            requests[0].reasoning.is_none(),
            "off thinking level must omit request.reasoning"
        );
    }

    #[test]
    fn run_initial_request_includes_reasoning_effort_when_thinking_is_enabled() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.3-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        provider
            .cycle_thinking_level()
            .expect("thinking cycling should be supported");

        let _events = run_events_with_executor(&provider, |_call| {
            panic!("tool executor should not be called when no tool calls are emitted")
        });

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.as_deref()),
            Some("minimal")
        );
        assert_eq!(
            requests[0]
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.summary.as_deref()),
            None
        );
    }

    #[test]
    fn run_initial_request_replays_full_message_history_in_stable_order() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 9,
                    messages: vec![
                        RunMessage::UserText {
                            text: "turn-1 user".to_string(),
                        },
                        RunMessage::AssistantText {
                            text: "turn-1 assistant".to_string(),
                        },
                        RunMessage::ToolCall {
                            call_id: "call_1".to_string(),
                            tool_name: "read".to_string(),
                            arguments: json!({ "path": "README.md" }),
                        },
                        RunMessage::ToolResult {
                            call_id: "call_1".to_string(),
                            tool_name: "read".to_string(),
                            content: json!("file contents"),
                            is_error: false,
                        },
                        RunMessage::UserText {
                            text: "turn-2 user".to_string(),
                        },
                    ],
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |_call| {
                    panic!("tool executor should not be called when stream does not request tools")
                },
                &mut |event| events.push(event),
            )
            .expect("run should succeed");

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 1);
        let initial_input = requests[0]
            .input
            .as_array()
            .expect("initial request input should be an array");

        assert_eq!(initial_input.len(), 5);
        assert_eq!(initial_input[0]["role"], "user");
        assert_eq!(initial_input[0]["content"][0]["type"], "input_text");
        assert_eq!(initial_input[0]["content"][0]["text"], "turn-1 user");

        assert_eq!(initial_input[1]["type"], "message");
        assert_eq!(initial_input[1]["role"], "assistant");
        assert_eq!(initial_input[1]["content"][0]["type"], "output_text");
        assert_eq!(initial_input[1]["content"][0]["text"], "turn-1 assistant");
        assert_eq!(initial_input[1]["status"], "completed");
        assert_eq!(initial_input[1]["id"], "msg_0");

        assert_eq!(initial_input[2]["type"], "function_call");
        assert_eq!(initial_input[2]["call_id"], "call_1");
        assert_eq!(initial_input[2]["name"], "read");
        assert_eq!(initial_input[2]["arguments"], "{\"path\":\"README.md\"}");

        assert_eq!(initial_input[3]["type"], "function_call_output");
        assert_eq!(initial_input[3]["call_id"], "call_1");
        assert_eq!(initial_input[3]["output"], "file contents");

        assert_eq!(initial_input[4]["role"], "user");
        assert_eq!(initial_input[4]["content"][0]["type"], "input_text");
        assert_eq!(initial_input[4]["content"][0]["text"], "turn-2 user");

        assert!(matches!(
            events.as_slice(),
            [
                RunEvent::Started { run_id: 9 },
                RunEvent::Finished { run_id: 9 }
            ]
        ));
    }

    #[test]
    fn run_fails_when_replayed_tool_call_arguments_are_not_object() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 9,
                messages: vec![
                    RunMessage::UserText {
                        text: "turn-1 user".to_string(),
                    },
                    RunMessage::ToolCall {
                        call_id: "call_1".to_string(),
                        tool_name: "read".to_string(),
                        arguments: json!("bad"),
                    },
                ],
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("invalid replayed tool call arguments should fail fast");
        assert!(error.contains("requires tool call arguments to be a JSON object"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
    }

    #[test]
    fn run_fails_when_replayed_tool_result_has_empty_call_id() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 9,
                messages: vec![
                    RunMessage::UserText {
                        text: "turn-1 user".to_string(),
                    },
                    RunMessage::ToolResult {
                        call_id: "   ".to_string(),
                        tool_name: "read".to_string(),
                        content: json!("out"),
                        is_error: false,
                    },
                ],
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("invalid replayed tool result should fail fast");
        assert!(error.contains("requires non-empty tool result call_id"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
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
        assert_transport_invariants(&requests[0], "system instructions");
        assert_transport_invariants(&requests[1], "system instructions");
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input.len(), 3);
        assert_eq!(follow_up_input[0]["role"], "user");
        assert_eq!(follow_up_input[0]["content"][0]["type"], "input_text");
        assert_eq!(follow_up_input[0]["content"][0]["text"], "hello");
        assert_eq!(follow_up_input[1]["type"], "function_call");
        assert_eq!(follow_up_input[1]["call_id"], "call_1");
        assert_eq!(follow_up_input[1]["id"], "fc_1");
        assert_eq!(follow_up_input[1]["name"], "read");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(follow_up_input[2]["call_id"], "call_1");
        assert_eq!(follow_up_input[2]["output"], "file contents");

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_tool_execution_uses_transport_call_id_but_replay_uses_canonical_call_id() {
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
                events: Vec::new(),
                terminal: Some(CodexResponseStatus::Completed),
            }),
        ]);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut executed_call_ids = Vec::new();
        let events = run_events_with_executor(&provider, |call| {
            executed_call_ids.push(call.call_id.clone());
            ToolResult::success("mismatched_call_id", call.tool_name, "tool output")
        });

        assert_eq!(executed_call_ids, vec!["call_1".to_string()]);

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input[1]["type"], "function_call");
        assert_eq!(follow_up_input[1]["call_id"], "call_1");
        assert_eq!(follow_up_input[1]["id"], "fc_1");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(follow_up_input[2]["call_id"], "call_1");
        assert_eq!(follow_up_input[2]["output"], "tool output");

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_follow_up_request_replays_prior_history_before_roundtrip_items() {
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
                events: Vec::new(),
                terminal: Some(CodexResponseStatus::Completed),
            }),
        ]);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 9,
                    messages: vec![
                        RunMessage::UserText {
                            text: "turn-1 user".to_string(),
                        },
                        RunMessage::AssistantText {
                            text: "turn-1 assistant".to_string(),
                        },
                        RunMessage::UserText {
                            text: "turn-2 user".to_string(),
                        },
                    ],
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |call| ToolResult::success(call.call_id, call.tool_name, "tool output"),
                &mut |event| events.push(event),
            )
            .expect("run should succeed");

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input.len(), 5);
        assert_eq!(follow_up_input[0]["role"], "user");
        assert_eq!(follow_up_input[0]["content"][0]["text"], "turn-1 user");
        assert_eq!(follow_up_input[1]["type"], "message");
        assert_eq!(follow_up_input[1]["role"], "assistant");
        assert_eq!(follow_up_input[1]["content"][0]["text"], "turn-1 assistant");
        assert_eq!(follow_up_input[2]["role"], "user");
        assert_eq!(follow_up_input[2]["content"][0]["text"], "turn-2 user");
        assert_eq!(follow_up_input[3]["type"], "function_call");
        assert_eq!(follow_up_input[3]["call_id"], "call_1");
        assert_eq!(follow_up_input[3]["id"], "fc_1");
        assert_eq!(follow_up_input[4]["type"], "function_call_output");
        assert_eq!(follow_up_input[4]["call_id"], "call_1");
        assert_eq!(follow_up_input[4]["output"], "tool output");

        assert!(matches!(
            events.as_slice(),
            [
                RunEvent::Started { run_id: 9 },
                RunEvent::Finished { run_id: 9 }
            ]
        ));
    }

    #[test]
    fn run_follow_up_replay_preserves_interleaved_text_and_tool_call_order() {
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![
                    CodexStreamEvent::OutputTextDelta {
                        delta: "A".to_string(),
                    },
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_1".to_string()),
                        call_id: Some("call_1".to_string()),
                        tool_name: Some("read".to_string()),
                        arguments: Some(Value::String("{\"path\":\"README.md\"}".to_string())),
                    },
                    CodexStreamEvent::OutputTextDelta {
                        delta: "B".to_string(),
                    },
                    CodexStreamEvent::ToolCallRequested {
                        id: Some("fc_2".to_string()),
                        call_id: Some("call_2".to_string()),
                        tool_name: Some("bash".to_string()),
                        arguments: Some(Value::String("{\"command\":\"pwd\"}".to_string())),
                    },
                ],
                terminal: Some(CodexResponseStatus::Completed),
            }),
            FakeStreamOutcome::Success(StreamResult {
                events: Vec::new(),
                terminal: Some(CodexResponseStatus::Completed),
            }),
        ]);
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let mut observed_call_ids = Vec::new();
        let events = run_events_with_executor(&provider, |call| {
            observed_call_ids.push(call.call_id.clone());
            ToolResult::success(
                call.call_id,
                call.tool_name,
                format!("result:{}", observed_call_ids.len()),
            )
        });

        assert_eq!(
            observed_call_ids,
            vec!["call_1".to_string(), "call_2".to_string()]
        );

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input.len(), 7);
        assert_eq!(follow_up_input[0]["role"], "user");
        assert_eq!(follow_up_input[0]["content"][0]["text"], "hello");
        assert_eq!(follow_up_input[1]["type"], "message");
        assert_eq!(follow_up_input[1]["content"][0]["text"], "A");
        assert_eq!(follow_up_input[2]["type"], "function_call");
        assert_eq!(follow_up_input[2]["call_id"], "call_1");
        assert_eq!(follow_up_input[2]["id"], "fc_1");
        assert_eq!(follow_up_input[3]["type"], "message");
        assert_eq!(follow_up_input[3]["content"][0]["text"], "B");
        assert_eq!(follow_up_input[4]["type"], "function_call");
        assert_eq!(follow_up_input[4]["call_id"], "call_2");
        assert_eq!(follow_up_input[4]["id"], "fc_2");
        assert_eq!(follow_up_input[5]["type"], "function_call_output");
        assert_eq!(follow_up_input[5]["call_id"], "call_1");
        assert_eq!(follow_up_input[5]["output"], "result:1");
        assert_eq!(follow_up_input[6]["type"], "function_call_output");
        assert_eq!(follow_up_input[6]["call_id"], "call_2");
        assert_eq!(follow_up_input[6]["output"], "result:2");

        assert!(events
            .iter()
            .any(|event| matches!(event, RunEvent::Chunk { text, .. } if text == "A")));
        assert!(events
            .iter()
            .any(|event| matches!(event, RunEvent::Chunk { text, .. } if text == "B")));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_roundtrips_apply_patch_success_call() {
        let patch_input =
            "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch";
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![CodexStreamEvent::ToolCallRequested {
                    id: Some("fc_apply_patch_1".to_string()),
                    call_id: Some("call_apply_patch_1".to_string()),
                    tool_name: Some("apply_patch".to_string()),
                    arguments: Some(Value::String(json!({ "input": patch_input }).to_string())),
                }],
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

        let mut observed_calls = Vec::new();
        let events = run_events_with_executor(&provider, |call| {
            observed_calls.push(call.clone());
            ToolResult::success(
                call.call_id,
                call.tool_name,
                "Success. Updated the following files:\nM src/main.rs",
            )
        });

        assert_eq!(observed_calls.len(), 1);
        assert_eq!(observed_calls[0].tool_name, "apply_patch");
        assert_eq!(observed_calls[0].arguments["input"], patch_input);

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].instructions.as_deref(),
            Some("system instructions"),
            "follow-up request must preserve instructions"
        );
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input[0]["role"], "user");
        assert_eq!(follow_up_input[0]["content"][0]["text"], "hello");
        assert_eq!(follow_up_input[1]["type"], "function_call");
        assert_eq!(follow_up_input[1]["name"], "apply_patch");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(
            follow_up_input[2]["output"],
            "Success. Updated the following files:\nM src/main.rs"
        );

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_roundtrips_apply_patch_malformed_failure_payload() {
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![CodexStreamEvent::ToolCallRequested {
                    id: Some("fc_apply_patch_bad".to_string()),
                    call_id: Some("call_apply_patch_bad".to_string()),
                    tool_name: Some("apply_patch".to_string()),
                    arguments: Some(Value::String(
                        json!({
                            "input": "*** Begin Patch\n*** Update File: foo.txt\n@@\n-old\n+new"
                        })
                        .to_string(),
                    )),
                }],
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

        let events = run_events_with_executor(&provider, |call| {
            ToolResult::error(
                call.call_id,
                call.tool_name,
                "apply_patch parse error: invalid patch",
            )
        });

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input[1]["name"], "apply_patch");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(
            follow_up_input[2]["output"],
            "apply_patch parse error: invalid patch"
        );

        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 9 })
        ));
    }

    #[test]
    fn run_roundtrips_apply_patch_path_escape_failure_payload() {
        let stream = FakeStreamClient::scripted(vec![
            FakeStreamOutcome::Success(StreamResult {
                events: vec![CodexStreamEvent::ToolCallRequested {
                    id: Some("fc_apply_patch_escape".to_string()),
                    call_id: Some("call_apply_patch_escape".to_string()),
                    tool_name: Some("apply_patch".to_string()),
                    arguments: Some(Value::String(
                        json!({
                            "input": "*** Begin Patch\n*** Add File: ../escape.txt\n+bad\n*** End Patch"
                        })
                        .to_string(),
                    )),
                }],
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

        let events = run_events_with_executor(&provider, |call| {
            ToolResult::error(
                call.call_id,
                call.tool_name,
                "apply_patch path escape rejected: Path escapes workspace root",
            )
        });

        let requests = stream.observed_requests();
        assert_eq!(requests.len(), 2);
        let follow_up_input = requests[1]
            .input
            .as_array()
            .expect("follow-up request input should be an array");
        assert_eq!(follow_up_input[1]["name"], "apply_patch");
        assert_eq!(follow_up_input[2]["type"], "function_call_output");
        assert_eq!(
            follow_up_input[2]["output"],
            "apply_patch path escape rejected: Path escapes workspace root"
        );

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
        assert_eq!(follow_up_input.len(), 5);
        assert_eq!(follow_up_input[0]["role"], "user");
        assert_eq!(follow_up_input[0]["content"][0]["text"], "hello");
        assert_eq!(follow_up_input[1]["type"], "function_call");
        assert_eq!(follow_up_input[1]["call_id"], "call_1");
        assert_eq!(follow_up_input[1]["id"], "fc_1");
        assert_eq!(follow_up_input[2]["type"], "function_call");
        assert_eq!(follow_up_input[2]["call_id"], "call_2");
        assert_eq!(follow_up_input[2]["id"], "fc_2");
        assert_eq!(follow_up_input[3]["type"], "function_call_output");
        assert_eq!(follow_up_input[3]["call_id"], "call_1");
        assert_eq!(follow_up_input[4]["type"], "function_call_output");
        assert_eq!(follow_up_input[4]["call_id"], "call_2");

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
                tool_name: Some("unknown_tool".to_string()),
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
            Some(RunEvent::Failed { run_id: 9, error }) if error.contains("Unsupported tool call 'unknown_tool'")
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
    fn run_rejects_empty_user_message_before_http_call() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 12,
                messages: vec![RunMessage::UserText {
                    text: "  \n\t ".to_string(),
                }],
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("empty user message should fail fast");
        assert!(error.contains("requires non-empty user text messages"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
    }

    #[test]
    fn run_rejects_empty_message_history_before_http_call() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 12,
                messages: Vec::new(),
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("empty message history should fail fast");
        assert!(error.contains("requires non-empty run message history"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
    }

    #[test]
    fn run_rejects_history_without_user_message_before_http_call() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 12,
                messages: vec![RunMessage::AssistantText {
                    text: "assistant only".to_string(),
                }],
                instructions: "system instructions".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("history without user message should fail fast");
        assert!(error.contains("requires at least one user text message"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
    }

    #[test]
    fn run_rejects_empty_instructions_before_http_call() {
        let stream = FakeStreamClient::success(StreamResult {
            events: Vec::new(),
            terminal: Some(CodexResponseStatus::Completed),
        });
        let provider = CodexApiProvider::with_stream_client_for_tests(
            vec!["gpt-5.1-codex".to_string()],
            Arc::clone(&stream) as Arc<dyn StreamClient>,
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let result = provider.run(
            RunRequest {
                run_id: 12,
                messages: vec![RunMessage::UserText {
                    text: "hello".to_string(),
                }],
                instructions: "   \n\t ".to_string(),
            },
            cancel,
            &mut |_call| ToolResult::error("unused", "unused", "unused"),
            &mut |event| events.push(event),
        );

        let error = result.expect_err("empty instructions should fail fast");
        assert!(error.contains("requires non-empty run instructions"));
        assert!(events.is_empty());
        assert!(stream.observed_requests().is_empty());
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
