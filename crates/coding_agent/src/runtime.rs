use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use session_store::{SessionEntry, SessionEntryKind, SessionStore};
use tape_tui::runtime::tui::{
    Command, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle,
};

use crate::app::{App, HostOps, Mode, RunId};
use crate::provider::{
    ProviderProfile, RunEvent, RunMessage, RunProvider, RunRequest, ToolCallRequest, ToolResult,
};
use crate::tools::{BuiltinToolExecutor, ToolCall, ToolExecutor, ToolOutput};

struct ActiveRun {
    run_id: RunId,
    cancel: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

struct SessionRecorder {
    store: SessionStore,
    next_entry_index: u64,
    entry_timestamp: String,
}

impl SessionRecorder {
    fn new(store: SessionStore) -> Self {
        let entry_timestamp = store.header().created_at.clone();

        Self {
            store,
            next_entry_index: 1,
            entry_timestamp,
        }
    }

    fn persist_user_turn(&mut self, text: &str) -> Result<(), String> {
        self.append_kind(
            SessionEntryKind::UserText {
                text: text.to_string(),
            },
            "user turn",
        )
    }

    fn persist_committed_entries(&mut self, entries: &[RunMessage]) -> Result<(), String> {
        for entry in entries {
            match entry {
                RunMessage::AssistantText { text } => self.append_kind(
                    SessionEntryKind::AssistantText { text: text.clone() },
                    "assistant turn",
                )?,
                RunMessage::ToolCall {
                    call_id,
                    tool_name,
                    arguments,
                } => self.append_kind(
                    SessionEntryKind::ToolCall {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                    },
                    "tool call",
                )?,
                RunMessage::ToolResult {
                    call_id,
                    tool_name,
                    content,
                    is_error,
                } => self.append_kind(
                    SessionEntryKind::ToolResult {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        content: content.clone(),
                        is_error: *is_error,
                    },
                    "tool result",
                )?,
                RunMessage::UserText { .. } => {}
            }
        }

        Ok(())
    }

    fn append_kind(&mut self, kind: SessionEntryKind, description: &str) -> Result<(), String> {
        let entry_id = format!("entry-{:020}", self.next_entry_index);
        self.next_entry_index = self.next_entry_index.saturating_add(1);

        let parent_id = self.store.current_leaf_id().map(ToOwned::to_owned);
        let entry = SessionEntry::new(entry_id, parent_id, self.entry_timestamp.clone(), kind);

        self.store.append(entry).map_err(|error| {
            format!(
                "Failed persisting {description} to session '{}': {error}",
                self.store.path().display()
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSwitchResult {
    Updated(ProviderProfile),
    RejectedWhileRunning,
    Failed(String),
}

#[derive(Debug, Clone)]
enum RuntimeEvent {
    Provider(RunEvent),
    ToolCallStarted {
        run_id: RunId,
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolCallCompleted {
        run_id: RunId,
        result: ToolResult,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BuiltinDispatchTool {
    Bash,
    Read,
    Edit,
    Write,
    ApplyPatch,
}

#[derive(Debug)]
enum HostToolExecutor {
    Ready(BuiltinToolExecutor),
    Unavailable(String),
}

impl HostToolExecutor {
    fn execute(&mut self, call: ToolCall) -> ToolOutput {
        match self {
            Self::Ready(executor) => executor.execute(call),
            Self::Unavailable(reason) => {
                ToolOutput::fail(format!("Host tool executor is unavailable: {reason}"))
            }
        }
    }
}

pub const POST_TERMINAL_TOOL_REJECTION_ERROR: &str =
    "Provider requested tool call after terminal run event";
pub const SESSION_PERSISTENCE_FATAL_ERROR_PREFIX: &str = "Session persistence failed:";

pub struct RuntimeController {
    app: Arc<Mutex<App>>,
    runtime_handle: RuntimeHandle,
    pending_events: Arc<Mutex<VecDeque<RuntimeEvent>>>,
    next_run_id: AtomicU64,
    active_run: Mutex<Option<ActiveRun>>,
    provider: Arc<dyn RunProvider>,
    provider_id: String,
    tool_dispatch: HashMap<(String, String), BuiltinDispatchTool>,
    host_tool_executor: Mutex<HostToolExecutor>,
    session_recorder: Option<Mutex<SessionRecorder>>,
}

impl RuntimeController {
    /// Creates a controller that buffers runtime events before applying them to `App`.
    ///
    /// In UI environments, events are drained by the runtime command path. In headless or
    /// non-polling environments, call [`RuntimeController::flush_pending_run_events`] after
    /// enqueuing events to ensure queued run state is applied.
    pub fn new(
        app: Arc<Mutex<App>>,
        runtime_handle: RuntimeHandle,
        provider: Arc<dyn RunProvider>,
    ) -> Arc<Self> {
        Self::new_with_optional_session_store(app, runtime_handle, provider, None)
    }

    /// Creates a controller with persistent session recording enabled.
    pub fn new_with_session_store(
        app: Arc<Mutex<App>>,
        runtime_handle: RuntimeHandle,
        provider: Arc<dyn RunProvider>,
        session_store: SessionStore,
    ) -> Arc<Self> {
        Self::new_with_optional_session_store(app, runtime_handle, provider, Some(session_store))
    }

    fn new_with_optional_session_store(
        app: Arc<Mutex<App>>,
        runtime_handle: RuntimeHandle,
        provider: Arc<dyn RunProvider>,
        session_store: Option<SessionStore>,
    ) -> Arc<Self> {
        let provider_id = provider.profile().provider_id;

        Arc::new(Self {
            app,
            runtime_handle,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            next_run_id: AtomicU64::new(1),
            active_run: Mutex::new(None),
            tool_dispatch: build_tool_dispatch_table(&provider_id),
            host_tool_executor: Mutex::new(build_default_host_tool_executor()),
            session_recorder: session_store.map(|store| Mutex::new(SessionRecorder::new(store))),
            provider,
            provider_id,
        })
    }

    fn start_run_internal(
        self: &Arc<Self>,
        messages: Vec<RunMessage>,
        base_system_instructions: String,
    ) -> Result<RunId, String> {
        let mut active_run = self.lock_active_run();
        if active_run.is_some() {
            return Err("Run already active".to_string());
        }

        if let Err(error) = self.persist_submitted_user_turn(&messages) {
            self.runtime_handle.dispatch(Command::RequestStop);
            return Err(format!("{SESSION_PERSISTENCE_FATAL_ERROR_PREFIX} {error}"));
        }

        let run_id = self.next_run_id.fetch_add(1, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        let instructions = compose_system_instructions(
            &base_system_instructions,
            tool_prompting_instruction_appendix(),
        )?;
        let request = RunRequest {
            run_id,
            messages,
            instructions,
        };
        let join_handle = self.spawn_worker(request, Arc::clone(&cancel))?;

        *active_run = Some(ActiveRun {
            run_id,
            cancel,
            join_handle: Some(join_handle),
        });

        Ok(run_id)
    }

    fn spawn_worker(
        self: &Arc<Self>,
        request: RunRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<JoinHandle<()>, String> {
        let run_id = request.run_id;
        let controller = Arc::clone(self);
        thread::Builder::new()
            .name(format!("coding-agent-run-{run_id}"))
            .spawn(move || controller.run_worker(request, cancel))
            .map_err(|error| format!("Failed to spawn run worker: {error}"))
    }

    fn run_worker(self: Arc<Self>, request: RunRequest, cancel: Arc<AtomicBool>) {
        let run_id = request.run_id;
        self.wait_for_app_run_visibility(run_id);

        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let terminal_emitted_for_emit = Arc::clone(&terminal_emitted);
        let terminal_emitted_for_tools = Arc::clone(&terminal_emitted);
        let controller_for_emit = Arc::clone(&self);
        let controller_for_tools = Arc::clone(&self);
        let provider = Arc::clone(&self.provider);
        let cancel_for_tools = Arc::clone(&cancel);

        let mut emit = move |event: RunEvent| {
            if event.is_terminal() {
                terminal_emitted_for_emit.store(true, Ordering::SeqCst);
            }

            controller_for_emit.enqueue_runtime_event(RuntimeEvent::Provider(event));
        };

        let mut execute_tool = move |call: ToolCallRequest| {
            controller_for_tools.dispatch_host_tool_call(
                run_id,
                &cancel_for_tools,
                &terminal_emitted_for_tools,
                call,
            )
        };

        let run_outcome = catch_unwind(AssertUnwindSafe(|| {
            provider.run(request, Arc::clone(&cancel), &mut execute_tool, &mut emit)
        }));

        match run_outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => emit(RunEvent::Failed { run_id, error }),
            Err(_) => emit(RunEvent::Failed {
                run_id,
                error: "Run provider panicked".to_string(),
            }),
        }

        if !terminal_emitted.load(Ordering::SeqCst) && self.is_active_run_id(run_id) {
            emit(RunEvent::Failed {
                run_id,
                error: "Run provider exited without terminal event".to_string(),
            });
        }
    }

    fn dispatch_host_tool_call(
        self: &Arc<Self>,
        run_id: RunId,
        cancel: &Arc<AtomicBool>,
        terminal_emitted: &Arc<AtomicBool>,
        call: ToolCallRequest,
    ) -> ToolResult {
        let call_id = call.call_id.clone();
        let tool_name = call.tool_name.clone();

        if terminal_emitted.load(Ordering::SeqCst) {
            return ToolResult::error(call_id, tool_name, POST_TERMINAL_TOOL_REJECTION_ERROR);
        }

        self.enqueue_runtime_event(RuntimeEvent::ToolCallStarted {
            run_id,
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            arguments: call.arguments.clone(),
        });

        if cancel.load(Ordering::SeqCst) {
            return self.finish_tool_call(
                run_id,
                ToolResult::error(
                    call_id,
                    tool_name,
                    "Run cancellation requested before host tool execution",
                ),
            );
        }

        let dispatch_key = (self.provider_id.clone(), tool_name.clone());
        let Some(dispatch_tool) = self.tool_dispatch.get(&dispatch_key).copied() else {
            let error = format!(
                "Unknown host tool '{tool_name}' for provider '{}'",
                self.provider_id
            );

            return self.finish_tool_call(run_id, ToolResult::error(call_id, tool_name, error));
        };

        let tool_call = match parse_tool_call(&call, dispatch_tool) {
            Ok(tool_call) => tool_call,
            Err(error) => {
                return self.finish_tool_call(run_id, ToolResult::error(call_id, tool_name, error));
            }
        };

        let tool_output = match catch_unwind(AssertUnwindSafe(|| {
            let mut executor = lock_unpoisoned(&self.host_tool_executor);
            executor.execute(tool_call)
        })) {
            Ok(output) => output,
            Err(_) => ToolOutput::fail("Host tool executor panicked".to_string()),
        };

        let mut result = if tool_output.ok {
            ToolResult::success(call_id.clone(), tool_name.clone(), tool_output.content)
        } else {
            ToolResult::error(call_id.clone(), tool_name.clone(), tool_output.content)
        };

        if cancel.load(Ordering::SeqCst) && !result.is_error {
            result = ToolResult::error(
                call_id,
                tool_name,
                "Run cancellation requested during host tool execution",
            );
        }

        self.finish_tool_call(run_id, result)
    }

    fn finish_tool_call(self: &Arc<Self>, run_id: RunId, result: ToolResult) -> ToolResult {
        self.enqueue_runtime_event(RuntimeEvent::ToolCallCompleted {
            run_id,
            result: result.clone(),
        });

        result
    }

    fn enqueue_runtime_event(self: &Arc<Self>, event: RuntimeEvent) {
        let should_drain = {
            let mut queue = lock_unpoisoned(&self.pending_events);
            let should_drain = queue.is_empty();
            queue.push_back(event);
            should_drain
        };

        if should_drain {
            self.runtime_handle
                .dispatch(Command::Custom(Box::new(DrainRunEventsCommand {
                    controller: Arc::clone(self),
                })));
        }
    }

    fn drain_pending_run_events(&self) -> usize {
        let mut drained = 0usize;

        loop {
            let event = {
                let mut pending_events = lock_unpoisoned(&self.pending_events);
                pending_events.pop_front()
            };

            match event {
                Some(event) => {
                    self.apply_runtime_event(event);
                    drained += 1;
                }
                None => break,
            }
        }

        drained
    }

    /// Drains queued runtime events and schedules a render.
    ///
    /// Use this in non-ticking environments (for example headless test
    /// harnesses or external callers that never call `RuntimeHandle::run_once`) to
    /// guarantee queued run and tool state is applied.
    pub fn flush_pending_run_events(&self) -> usize {
        let drained = self.drain_pending_run_events();
        if drained > 0 {
            self.runtime_handle.dispatch(Command::RequestRender);
        }

        drained
    }

    fn wait_for_app_run_visibility(&self, run_id: RunId) {
        for _ in 0..256 {
            let run_visible = {
                let app = lock_unpoisoned(&self.app);
                matches!(app.mode, Mode::Running { run_id: current } if current == run_id)
            };

            if run_visible {
                return;
            }

            thread::yield_now();
        }
    }

    fn apply_runtime_event(&self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Provider(event) => self.apply_provider_run_event(event),
            RuntimeEvent::ToolCallStarted {
                run_id,
                call_id,
                tool_name,
                arguments,
            } => {
                let mut app = lock_unpoisoned(&self.app);
                app.on_tool_call_started(run_id, &call_id, &tool_name, &arguments);
            }
            RuntimeEvent::ToolCallCompleted { run_id, result } => {
                let content = tool_result_content_as_text(&result.content);
                let mut app = lock_unpoisoned(&self.app);
                app.on_tool_call_finished(
                    run_id,
                    &result.tool_name,
                    &result.call_id,
                    result.is_error,
                    &result.content,
                    &content,
                );
            }
        }
    }

    fn apply_provider_run_event(&self, event: RunEvent) {
        let run_id = event.run_id();
        let terminal = event.is_terminal();
        let persist_finished_entries = matches!(event, RunEvent::Finished { .. });
        let mut committed_entries = Vec::new();

        {
            let mut app = lock_unpoisoned(&self.app);
            let conversation_len_before = if persist_finished_entries {
                app.conversation_messages().len()
            } else {
                0
            };

            match event {
                RunEvent::Started { run_id } => app.on_run_started(run_id),
                RunEvent::Chunk { run_id, text } => app.on_run_chunk(run_id, &text),
                RunEvent::Finished { run_id } => app.on_run_finished(run_id),
                RunEvent::Failed { run_id, error } => app.on_run_failed(run_id, &error),
                RunEvent::Cancelled { run_id } => app.on_run_cancelled(run_id),
            }

            if persist_finished_entries {
                committed_entries = committed_assistant_and_tool_entries(
                    app.conversation_messages(),
                    conversation_len_before,
                );
            }
        }

        if persist_finished_entries && !committed_entries.is_empty() {
            if let Err(error) = self.persist_committed_entries(&committed_entries) {
                self.handle_persistence_failure(error);
            }
        }

        if terminal {
            self.clear_active_run_if_matching(run_id);
        }
    }

    fn persist_submitted_user_turn(&self, messages: &[RunMessage]) -> Result<(), String> {
        let Some(RunMessage::UserText { text }) = messages.last() else {
            return Ok(());
        };

        self.persist_user_turn(text)
    }

    fn persist_user_turn(&self, text: &str) -> Result<(), String> {
        let Some(session_recorder) = self.session_recorder.as_ref() else {
            return Ok(());
        };

        let mut session_recorder = lock_unpoisoned(session_recorder);
        session_recorder.persist_user_turn(text)
    }

    fn persist_committed_entries(&self, entries: &[RunMessage]) -> Result<(), String> {
        let Some(session_recorder) = self.session_recorder.as_ref() else {
            return Ok(());
        };

        let mut session_recorder = lock_unpoisoned(session_recorder);
        session_recorder.persist_committed_entries(entries)
    }

    fn handle_persistence_failure(&self, error: String) {
        {
            let mut app = lock_unpoisoned(&self.app);
            app.mode = Mode::Error(error.clone());
            app.push_system_message(format!("Session persistence failed: {error}"));
            app.should_exit = true;
        }

        self.runtime_handle.dispatch(Command::RequestStop);
        self.runtime_handle.dispatch(Command::RequestRender);
    }

    fn clear_active_run_if_matching(&self, run_id: RunId) {
        let mut active_run = self.lock_active_run();
        let matches = active_run.as_ref().map(|active| active.run_id) == Some(run_id);
        if !matches {
            return;
        }

        let mut completed = match active_run.take() {
            Some(completed) => completed,
            None => return,
        };

        if let Some(join_handle) = completed.join_handle.take() {
            let is_current_thread = join_handle.thread().id() == thread::current().id();
            if !is_current_thread && join_handle.is_finished() {
                let _ = join_handle.join();
            }
        }
    }

    fn is_active_run_id(&self, run_id: RunId) -> bool {
        self.lock_active_run().as_ref().map(|active| active.run_id) == Some(run_id)
    }

    fn cancel_run_internal(&self, run_id: RunId) {
        let active_run = self.lock_active_run();
        if let Some(active_run) = active_run.as_ref() {
            if active_run.run_id == run_id {
                active_run.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn cycle_model_profile(&self) -> ProfileSwitchResult {
        let active_run = self.lock_active_run();
        if active_run.is_some() {
            return ProfileSwitchResult::RejectedWhileRunning;
        }

        match self.provider.cycle_model() {
            Ok(profile) => ProfileSwitchResult::Updated(profile),
            Err(error) => ProfileSwitchResult::Failed(error),
        }
    }

    pub fn cycle_thinking_profile(&self) -> ProfileSwitchResult {
        let active_run = self.lock_active_run();
        if active_run.is_some() {
            return ProfileSwitchResult::RejectedWhileRunning;
        }

        match self.provider.cycle_thinking_level() {
            Ok(profile) => ProfileSwitchResult::Updated(profile),
            Err(error) => ProfileSwitchResult::Failed(error),
        }
    }

    fn lock_active_run(&self) -> MutexGuard<'_, Option<ActiveRun>> {
        lock_unpoisoned(&self.active_run)
    }
}

struct DrainRunEventsCommand {
    controller: Arc<RuntimeController>,
}

impl CustomCommand for DrainRunEventsCommand {
    fn name(&self) -> &'static str {
        "drain_run_events"
    }

    fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
        let drained = self.controller.drain_pending_run_events();
        if drained > 0 {
            ctx.request_render();
        }
        Ok(())
    }
}

impl HostOps for Arc<RuntimeController> {
    fn start_run(
        &mut self,
        messages: Vec<RunMessage>,
        instructions: String,
    ) -> Result<RunId, String> {
        self.start_run_internal(messages, instructions)
    }

    fn cancel_run(&mut self, run_id: RunId) {
        self.cancel_run_internal(run_id);
    }

    fn request_render(&mut self) {
        self.runtime_handle.dispatch(Command::RequestRender);
    }

    fn request_stop(&mut self) {
        self.runtime_handle.dispatch(Command::RequestStop);
    }
}

fn compose_system_instructions(base: &str, tool_appendix: &str) -> Result<String, String> {
    let base = base.trim();
    if base.is_empty() {
        return Err("System instructions cannot be empty".to_string());
    }

    let tool_appendix = tool_appendix.trim();
    if tool_appendix.is_empty() {
        return Err("Tool prompting appendix cannot be empty".to_string());
    }

    Ok(format!("{base}\n\n{tool_appendix}"))
}

fn tool_prompting_instruction_appendix() -> &'static str {
    "Tool use policy:\n- Use tools for workspace actions: read, bash, edit, write, apply_patch.\n- Prefer the smallest safe tool for the step you are performing.\n- Never fabricate tool success; report explicit tool errors as-is.\n- Keep mutating changes minimal and verifiable.\n- Do not substitute fallback providers or hidden behavior when provider/tool errors occur."
}

fn build_default_host_tool_executor() -> HostToolExecutor {
    let workspace_root = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            return HostToolExecutor::Unavailable(format!(
                "Failed to resolve current working directory: {error}"
            ));
        }
    };

    match BuiltinToolExecutor::new(workspace_root) {
        Ok(executor) => HostToolExecutor::Ready(executor),
        Err(error) => HostToolExecutor::Unavailable(error),
    }
}

fn build_tool_dispatch_table(provider_id: &str) -> HashMap<(String, String), BuiltinDispatchTool> {
    let provider_id = provider_id.to_string();

    HashMap::from([
        (
            (provider_id.clone(), "bash".to_string()),
            BuiltinDispatchTool::Bash,
        ),
        (
            (provider_id.clone(), "read".to_string()),
            BuiltinDispatchTool::Read,
        ),
        (
            (provider_id.clone(), "edit".to_string()),
            BuiltinDispatchTool::Edit,
        ),
        (
            (provider_id.clone(), "write".to_string()),
            BuiltinDispatchTool::Write,
        ),
        (
            (provider_id, "apply_patch".to_string()),
            BuiltinDispatchTool::ApplyPatch,
        ),
    ])
}

fn parse_tool_call(
    call: &ToolCallRequest,
    dispatch_tool: BuiltinDispatchTool,
) -> Result<ToolCall, String> {
    let args = args_object(&call.tool_name, &call.arguments)?;

    match dispatch_tool {
        BuiltinDispatchTool::Bash => Ok(ToolCall::Bash {
            command: required_string_arg(args, &call.tool_name, "command")?,
            timeout_sec: optional_u64_arg(args, &call.tool_name, "timeout_sec")?,
            cwd: optional_string_arg(args, &call.tool_name, "cwd")?,
        }),
        BuiltinDispatchTool::Read => Ok(ToolCall::ReadFile {
            path: required_string_arg(args, &call.tool_name, "path")?,
        }),
        BuiltinDispatchTool::Edit => Ok(ToolCall::EditFile {
            path: required_string_arg(args, &call.tool_name, "path")?,
            old_text: required_string_arg(args, &call.tool_name, "old_text")?,
            new_text: required_string_arg(args, &call.tool_name, "new_text")?,
        }),
        BuiltinDispatchTool::Write => Ok(ToolCall::WriteFile {
            path: required_string_arg(args, &call.tool_name, "path")?,
            content: required_string_arg(args, &call.tool_name, "content")?,
        }),
        BuiltinDispatchTool::ApplyPatch => Ok(ToolCall::ApplyPatch {
            input: required_string_arg(args, &call.tool_name, "input")?,
        }),
    }
}

fn args_object<'a>(
    tool_name: &str,
    args: &'a Value,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    args.as_object()
        .ok_or_else(|| format!("Invalid arguments for tool '{tool_name}': expected a JSON object"))
}

fn required_string_arg(
    args: &serde_json::Map<String, Value>,
    tool_name: &str,
    field: &str,
) -> Result<String, String> {
    match args.get(field) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!(
            "Invalid arguments for tool '{tool_name}': field '{field}' must be a string"
        )),
        None => Err(format!(
            "Invalid arguments for tool '{tool_name}': missing required field '{field}'"
        )),
    }
}

fn optional_string_arg(
    args: &serde_json::Map<String, Value>,
    tool_name: &str,
    field: &str,
) -> Result<Option<String>, String> {
    match args.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "Invalid arguments for tool '{tool_name}': optional field '{field}' must be a string"
        )),
        None => Ok(None),
    }
}

fn optional_u64_arg(
    args: &serde_json::Map<String, Value>,
    tool_name: &str,
    field: &str,
) -> Result<Option<u64>, String> {
    match args.get(field) {
        Some(Value::Number(value)) => value.as_u64().map(Some).ok_or_else(|| {
            format!(
                "Invalid arguments for tool '{tool_name}': optional field '{field}' must be an unsigned integer"
            )
        }),
        Some(_) => Err(format!(
            "Invalid arguments for tool '{tool_name}': optional field '{field}' must be an unsigned integer"
        )),
        None => Ok(None),
    }
}

fn tool_result_content_as_text(value: &Value) -> String {
    match value {
        Value::String(content) => content.clone(),
        other => other.to_string(),
    }
}

fn committed_assistant_and_tool_entries(
    conversation: &[RunMessage],
    start_index: usize,
) -> Vec<RunMessage> {
    let start_index = start_index.min(conversation.len());

    conversation[start_index..]
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                RunMessage::AssistantText { .. }
                    | RunMessage::ToolCall { .. }
                    | RunMessage::ToolResult { .. }
            )
        })
        .cloned()
        .collect()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::{compose_system_instructions, tool_prompting_instruction_appendix};

    #[test]
    fn composed_system_instructions_include_tool_policy_and_inventory() {
        let composed = compose_system_instructions(
            "Be deterministic and concise.",
            tool_prompting_instruction_appendix(),
        )
        .expect("composition should succeed");

        assert!(composed.contains("Be deterministic and concise."));
        assert!(composed.contains("Tool use policy:"));
        assert!(composed.contains("read"));
        assert!(composed.contains("bash"));
        assert!(composed.contains("edit"));
        assert!(composed.contains("write"));
        assert!(composed.contains("apply_patch"));
        assert!(!composed.trim().is_empty());
    }

    #[test]
    fn composed_system_instructions_reject_empty_base() {
        let error = compose_system_instructions("   ", tool_prompting_instruction_appendix())
            .expect_err("empty base instructions should fail");

        assert!(error.contains("cannot be empty"));
    }
}
