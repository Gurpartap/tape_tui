use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use coding_agent::app::{App, Mode, Role, RunId};
use coding_agent::provider::{
    CancelSignal, ProviderProfile, RunEvent, RunMessage, RunProvider, RunRequest, ToolCallRequest,
    ToolResult,
};
use coding_agent::runtime::{RuntimeController, POST_TERMINAL_TOOL_REJECTION_ERROR};
use tape_tui::{Terminal, TUI};

#[derive(Default)]
struct NullTerminal;

impl Terminal for NullTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(String) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}

    fn write(&mut self, _data: &str) {}

    fn columns(&self) -> u16 {
        120
    }

    fn rows(&self) -> u16 {
        40
    }
}

fn test_provider_profile() -> ProviderProfile {
    ProviderProfile {
        provider_id: "contract-test".to_string(),
        model_id: "contract-model".to_string(),
        thinking_level: Some("balanced".to_string()),
    }
}

fn tool_result_content_text(result: &ToolResult) -> String {
    result
        .content
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| result.content.to_string())
}

struct LifecycleProvider;

impl RunProvider for LifecycleProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: "hello ".to_string(),
        });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: "world".to_string(),
        });
        emit(RunEvent::Finished { run_id: req.run_id });
        Ok(())
    }
}

struct NoisyTerminalProvider;

impl RunProvider for NoisyTerminalProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: "stable output".to_string(),
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        // These are intentionally invalid post-terminal events for the same run.
        emit(RunEvent::Failed {
            run_id: req.run_id,
            error: "late failure should be ignored".to_string(),
        });
        emit(RunEvent::Cancelled { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: " and trailing chunk".to_string(),
        });

        Ok(())
    }
}

struct CancelAwareProvider {
    cancel_observed: Arc<AtomicBool>,
}

impl CancelAwareProvider {
    fn new(cancel_observed: Arc<AtomicBool>) -> Self {
        Self { cancel_observed }
    }
}

impl RunProvider for CancelAwareProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: "streaming".to_string(),
        });

        while !cancel.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }

        self.cancel_observed.store(true, Ordering::SeqCst);
        emit(RunEvent::Cancelled { run_id: req.run_id });
        Ok(())
    }
}

struct StaleEventProvider;

impl RunProvider for StaleEventProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let stale_run_id = req.run_id + 10_000;

        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: stale_run_id,
            text: "stale-before ".to_string(),
        });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: "live-output".to_string(),
        });
        emit(RunEvent::Finished {
            run_id: stale_run_id,
        });
        emit(RunEvent::Chunk {
            run_id: stale_run_id,
            text: "stale-after".to_string(),
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        Ok(())
    }
}

struct UnknownToolProvider;

impl RunProvider for UnknownToolProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        emit(RunEvent::Started { run_id: req.run_id });

        let result = execute_tool(ToolCallRequest {
            call_id: "call-unknown".to_string(),
            tool_name: "not-a-tool".to_string(),
            arguments: json!({}),
        });

        if !result.is_error {
            return Err("expected unknown tool result to be an explicit error".to_string());
        }

        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: format!("unknown-tool-error:{}", tool_result_content_text(&result)),
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        Ok(())
    }
}

struct ExecutionFailureToolProvider;

impl RunProvider for ExecutionFailureToolProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        emit(RunEvent::Started { run_id: req.run_id });

        let result = execute_tool(ToolCallRequest {
            call_id: "call-exec-failure".to_string(),
            tool_name: "bash".to_string(),
            arguments: json!({
                "command": "echo 'boom' 1>&2; exit 9",
                "cwd": "."
            }),
        });

        if !result.is_error {
            return Err("expected tool execution failure to return explicit error".to_string());
        }

        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: format!("execution-error:{}", tool_result_content_text(&result)),
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        Ok(())
    }
}

struct FailThenCaptureProvider {
    turn: Mutex<u32>,
    captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
}

impl FailThenCaptureProvider {
    fn new(captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>) -> Self {
        Self {
            turn: Mutex::new(0),
            captured_second_turn_messages,
        }
    }
}

impl RunProvider for FailThenCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let turn = {
            let mut turn = lock_unpoisoned(&self.turn);
            *turn += 1;
            *turn
        };

        emit(RunEvent::Started { run_id: req.run_id });

        if turn == 1 {
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "partial failure output".to_string(),
            });

            let tool_result = execute_tool(ToolCallRequest {
                call_id: "call-failed-run-memory".to_string(),
                tool_name: "not-a-tool".to_string(),
                arguments: json!({}),
            });
            if !tool_result.is_error {
                return Err("expected failed-run tool call to return explicit error".to_string());
            }

            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: " after tool".to_string(),
            });
            emit(RunEvent::Failed {
                run_id: req.run_id,
                error: "boom".to_string(),
            });
        } else {
            *lock_unpoisoned(&self.captured_second_turn_messages) = Some(req.messages.clone());
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "ok".to_string(),
            });
            emit(RunEvent::Finished { run_id: req.run_id });
        }

        Ok(())
    }
}

struct CancelThenCaptureProvider {
    turn: Mutex<u32>,
    cancel_observed: Arc<AtomicBool>,
    captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
}

impl CancelThenCaptureProvider {
    fn new(
        cancel_observed: Arc<AtomicBool>,
        captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
    ) -> Self {
        Self {
            turn: Mutex::new(0),
            cancel_observed,
            captured_second_turn_messages,
        }
    }
}

impl RunProvider for CancelThenCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let turn = {
            let mut turn = lock_unpoisoned(&self.turn);
            *turn += 1;
            *turn
        };

        emit(RunEvent::Started { run_id: req.run_id });

        if turn == 1 {
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "partial cancel output".to_string(),
            });

            let tool_result = execute_tool(ToolCallRequest {
                call_id: "call-cancelled-run-memory".to_string(),
                tool_name: "not-a-tool".to_string(),
                arguments: json!({}),
            });
            if !tool_result.is_error {
                return Err("expected cancelled-run tool call to return explicit error".to_string());
            }

            while !cancel.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }

            self.cancel_observed.store(true, Ordering::SeqCst);
            emit(RunEvent::Cancelled { run_id: req.run_id });
        } else {
            *lock_unpoisoned(&self.captured_second_turn_messages) = Some(req.messages.clone());
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "ok".to_string(),
            });
            emit(RunEvent::Finished { run_id: req.run_id });
        }

        Ok(())
    }
}

struct ToolFlowWithStaleProviderEvents;

impl RunProvider for ToolFlowWithStaleProviderEvents {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let stale_run_id = req.run_id + 10_000;

        emit(RunEvent::Started { run_id: req.run_id });

        let result = execute_tool(ToolCallRequest {
            call_id: "call-stale-scope".to_string(),
            tool_name: "not-a-tool".to_string(),
            arguments: json!({}),
        });

        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: format!("tool-scope:{}", tool_result_content_text(&result)),
        });

        emit(RunEvent::Chunk {
            run_id: stale_run_id,
            text: "stale provider chunk".to_string(),
        });
        emit(RunEvent::Finished {
            run_id: stale_run_id,
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        Ok(())
    }
}

struct ApplyPatchToolFlowWithStaleProviderEvents;

impl RunProvider for ApplyPatchToolFlowWithStaleProviderEvents {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let stale_run_id = req.run_id + 10_000;

        emit(RunEvent::Started { run_id: req.run_id });

        let result = execute_tool(ToolCallRequest {
            call_id: "call-stale-apply-patch".to_string(),
            tool_name: "apply_patch".to_string(),
            arguments: json!({
                "input": "*** Begin Patch\n*** Add File: broken.txt\n+oops"
            }),
        });

        if !result.is_error {
            return Err("expected malformed apply_patch call to return explicit error".to_string());
        }

        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: format!("apply-patch-scope:{}", tool_result_content_text(&result)),
        });

        emit(RunEvent::Chunk {
            run_id: stale_run_id,
            text: "stale apply_patch provider chunk".to_string(),
        });
        emit(RunEvent::Finished {
            run_id: stale_run_id,
        });
        emit(RunEvent::Finished { run_id: req.run_id });

        Ok(())
    }
}

struct RuntimeLoopHandle {
    runtime: TUI<NullTerminal>,
    stopped: bool,
}

impl RuntimeLoopHandle {
    fn new() -> Self {
        let mut runtime = TUI::new(NullTerminal);
        runtime.start().expect("runtime start should succeed");

        Self {
            runtime,
            stopped: false,
        }
    }

    fn runtime_handle(&self) -> tape_tui::runtime::tui::RuntimeHandle {
        self.runtime.runtime_handle()
    }

    fn tick(&mut self) {
        self.runtime.run_once();
    }

    fn shutdown(&mut self) {
        if self.stopped {
            return;
        }

        self.stopped = true;
        let _ = self.runtime.stop();
    }
}

impl Drop for RuntimeLoopHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn with_runtime_loop<T>(f: impl FnOnce(&mut RuntimeLoopHandle) -> T) -> T {
    let mut runtime_loop = RuntimeLoopHandle::new();
    let result = catch_unwind(AssertUnwindSafe(|| f(&mut runtime_loop)));
    runtime_loop.shutdown();

    match result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_until(
    timeout: Duration,
    mut tick: impl FnMut(),
    mut predicate: impl FnMut() -> bool,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        tick();
        if predicate() {
            return true;
        }

        thread::sleep(Duration::from_millis(10));
    }

    tick();
    predicate()
}

fn submit_prompt(app: &Arc<Mutex<App>>, host: &mut Arc<RuntimeController>, prompt: &str) -> RunId {
    let mut app = lock_unpoisoned(app);
    app.on_input_replace(prompt.to_string());
    app.on_submit(host);

    match app.mode {
        Mode::Running { run_id } => run_id,
        _ => panic!("expected running mode after submit, got {:?}", app.mode),
    }
}

#[test]
fn provider_lifecycle_transitions_to_single_completed_assistant_message() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(LifecycleProvider);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "verify lifecycle");
        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message.content == "hello world"
                            && !message.streaming
                    })
            },
        );
        assert!(settled, "lifecycle run did not settle");

        let app = lock_unpoisoned(&app);
        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();

        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "hello world");
        assert!(!assistant_messages[0].streaming);
        assert!(!app.transcript.iter().any(
            |message| message.role == Role::System && message.content.starts_with("Run failed")
        ));
    });
}

#[test]
fn terminal_state_remains_stable_when_provider_emits_extra_terminal_events() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(NoisyTerminalProvider);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let first_run_id = submit_prompt(&app, &mut host, "first noisy run");
        let first_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(first_run_id)
                            && message.content == "stable output"
                    })
            },
        );
        assert!(first_settled, "first noisy run did not settle");

        let second_run_id = submit_prompt(&app, &mut host, "second noisy run");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == "stable output"
                    })
            },
        );
        assert!(second_settled, "second noisy run did not settle");
        assert!(second_run_id > first_run_id);

        let app = lock_unpoisoned(&app);
        for run_id in [first_run_id, second_run_id] {
            let assistant_messages: Vec<_> = app
                .transcript
                .iter()
                .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
                .collect();
            assert_eq!(assistant_messages.len(), 1);
            assert_eq!(assistant_messages[0].content, "stable output");
            assert!(!assistant_messages[0].streaming);
        }

        assert!(!app.transcript.iter().any(
            |message| message.role == Role::System && message.content.starts_with("Run failed")
        ));
        assert!(!app
            .transcript
            .iter()
            .any(|message| message.role == Role::System && message.content == "Run cancelled"));
    });
}

#[test]
fn cancellation_signal_reaches_provider_and_preserves_cancelled_state() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let cancel_observed = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn RunProvider> =
            Arc::new(CancelAwareProvider::new(Arc::clone(&cancel_observed)));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "cancel this run");

        {
            let mut app = lock_unpoisoned(&app);
            app.on_cancel(&mut host);
        }

        let settled = wait_until(
            Duration::from_secs(3),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                cancel_observed.load(Ordering::SeqCst) && {
                    let app = lock_unpoisoned(&app);
                    matches!(app.mode, Mode::Idle)
                        && app.transcript.iter().any(|message| {
                            message.role == Role::System && message.content == "Run cancelled"
                        })
                        && app.transcript.iter().any(|message| {
                            message.role == Role::Assistant && message.run_id == Some(run_id)
                        })
                }
            },
        );
        assert!(settled, "cancelled run did not settle");

        let app = lock_unpoisoned(&app);
        let cancelled_messages = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::System && message.content == "Run cancelled")
            .count();
        assert_eq!(cancelled_messages, 1);

        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "streaming");
        assert!(!assistant_messages[0].streaming);

        assert_eq!(
            app.conversation_messages(),
            &[RunMessage::UserText {
                text: "cancel this run".to_string(),
            }]
        );
    });
}

#[test]
fn failed_run_does_not_replay_assistant_or_tool_messages_on_next_turn() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let captured_second_turn_messages = Arc::new(Mutex::new(None));
        let provider: Arc<dyn RunProvider> = Arc::new(FailThenCaptureProvider::new(Arc::clone(
            &captured_second_turn_messages,
        )));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let _first_run_id = submit_prompt(&app, &mut host, "first prompt");
        let first_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Error(_))
                    && app.transcript.iter().any(|message| {
                        message.role == Role::System && message.content == "Run failed: boom"
                    })
            },
        );
        assert!(first_settled, "failed run did not settle");

        let second_run_id = submit_prompt(&app, &mut host, "second prompt");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == "ok"
                    })
            },
        );
        assert!(second_settled, "second run after failure did not settle");

        let captured = lock_unpoisoned(&captured_second_turn_messages)
            .clone()
            .expect("second turn messages should be captured");
        assert_eq!(
            captured,
            vec![
                RunMessage::UserText {
                    text: "first prompt".to_string(),
                },
                RunMessage::UserText {
                    text: "second prompt".to_string(),
                },
            ]
        );
    });
}

#[test]
fn cancelled_run_does_not_replay_assistant_or_tool_messages_on_next_turn() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let cancel_observed = Arc::new(AtomicBool::new(false));
        let captured_second_turn_messages = Arc::new(Mutex::new(None));
        let provider: Arc<dyn RunProvider> = Arc::new(CancelThenCaptureProvider::new(
            Arc::clone(&cancel_observed),
            Arc::clone(&captured_second_turn_messages),
        ));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let _first_run_id = submit_prompt(&app, &mut host, "first prompt");

        {
            let mut app = lock_unpoisoned(&app);
            app.on_cancel(&mut host);
        }

        let first_settled = wait_until(
            Duration::from_secs(3),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                cancel_observed.load(Ordering::SeqCst) && {
                    let app = lock_unpoisoned(&app);
                    matches!(app.mode, Mode::Idle)
                        && app.transcript.iter().any(|message| {
                            message.role == Role::System && message.content == "Run cancelled"
                        })
                }
            },
        );
        assert!(first_settled, "cancelled run did not settle");

        let second_run_id = submit_prompt(&app, &mut host, "second prompt");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == "ok"
                    })
            },
        );
        assert!(
            second_settled,
            "second run after cancellation did not settle"
        );

        let captured = lock_unpoisoned(&captured_second_turn_messages)
            .clone()
            .expect("second turn messages should be captured");
        assert_eq!(
            captured,
            vec![
                RunMessage::UserText {
                    text: "first prompt".to_string(),
                },
                RunMessage::UserText {
                    text: "second prompt".to_string(),
                },
            ]
        );
    });
}

#[test]
fn stale_run_events_are_ignored_and_do_not_corrupt_active_run_output() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(StaleEventProvider);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "stale event guard");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message.content == "live-output"
                            && !message.streaming
                    })
            },
        );
        assert!(settled, "run with stale events did not settle");

        let app = lock_unpoisoned(&app);
        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();

        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "live-output");
        assert!(!assistant_messages[0].streaming);
        assert!(!app
            .transcript
            .iter()
            .any(|message| message.content.contains("stale-before")
                || message.content.contains("stale-after")));
    });
}

#[test]
fn tool_timeline_stays_scoped_to_active_run_when_provider_emits_stale_events() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(ToolFlowWithStaleProviderEvents);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "tool stale scope");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message.content.contains("tool-scope:Unknown host tool")
                    })
            },
        );
        assert!(settled, "tool+stale run did not settle");

        let app = lock_unpoisoned(&app);
        let tool_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Tool)
            .collect();
        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0].run_id, Some(run_id));
        assert_eq!(
            tool_messages[0].content,
            "Tool not-a-tool (call-stale-scope) started"
        );
        assert_eq!(tool_messages[1].run_id, Some(run_id));
        assert!(tool_messages[1]
            .content
            .contains("Tool not-a-tool (call-stale-scope) failed: Unknown host tool"));
        assert!(!app
            .transcript
            .iter()
            .any(|message| message.content.contains("stale provider chunk")));

        let model_tool_messages: Vec<_> = app
            .conversation_messages()
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    RunMessage::ToolCall { .. } | RunMessage::ToolResult { .. }
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            model_tool_messages,
            vec![
                RunMessage::ToolCall {
                    call_id: "call-stale-scope".to_string(),
                    tool_name: "not-a-tool".to_string(),
                    arguments: json!({}),
                },
                RunMessage::ToolResult {
                    call_id: "call-stale-scope".to_string(),
                    tool_name: "not-a-tool".to_string(),
                    content: json!("Unknown host tool 'not-a-tool' for provider 'contract-test'"),
                    is_error: true,
                },
            ]
        );
    });
}

#[test]
fn apply_patch_tool_timeline_stays_scoped_to_active_run_when_provider_emits_stale_events() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(ApplyPatchToolFlowWithStaleProviderEvents);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "apply_patch stale scope");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message
                                .content
                                .contains("apply-patch-scope:apply_patch parse error")
                    })
            },
        );
        assert!(settled, "apply_patch+stale run did not settle");

        let app = lock_unpoisoned(&app);
        let tool_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Tool)
            .collect();
        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0].run_id, Some(run_id));
        assert_eq!(
            tool_messages[0].content,
            "Tool apply_patch (call-stale-apply-patch) started"
        );
        assert_eq!(tool_messages[1].run_id, Some(run_id));
        assert!(tool_messages[1]
            .content
            .contains("Tool apply_patch (call-stale-apply-patch) failed: apply_patch parse error"));
        assert!(!app
            .transcript
            .iter()
            .any(|message| message.content.contains("stale apply_patch provider chunk")));

        let model_tool_messages: Vec<_> = app
            .conversation_messages()
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    RunMessage::ToolCall { .. } | RunMessage::ToolResult { .. }
                )
            })
            .cloned()
            .collect();
        assert_eq!(model_tool_messages.len(), 2);
        assert_eq!(
            model_tool_messages[0],
            RunMessage::ToolCall {
                call_id: "call-stale-apply-patch".to_string(),
                tool_name: "apply_patch".to_string(),
                arguments: json!({
                    "input": "*** Begin Patch\n*** Add File: broken.txt\n+oops"
                }),
            }
        );

        let RunMessage::ToolResult {
            call_id,
            tool_name,
            content,
            is_error,
        } = &model_tool_messages[1]
        else {
            panic!("expected tool result message");
        };
        assert_eq!(call_id, "call-stale-apply-patch");
        assert_eq!(tool_name, "apply_patch");
        assert!(*is_error);
        let content = content
            .as_str()
            .expect("apply_patch stale tool result content should be string");
        assert!(content.starts_with("apply_patch parse error:"), "{content}");
    });
}

#[test]
fn unknown_tool_call_produces_explicit_error_result_and_tool_failure_timeline() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(UnknownToolProvider);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "unknown tool call");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message
                                .content
                                .contains("unknown-tool-error:Unknown host tool")
                    })
            },
        );
        assert!(settled, "run with unknown tool did not settle");

        let app = lock_unpoisoned(&app);
        assert!(app.transcript.iter().any(|message| {
            message.role == Role::Tool
                && message.content == "Tool not-a-tool (call-unknown) started"
        }));
        assert!(app.transcript.iter().any(|message| {
            message.role == Role::Tool
                && message
                    .content
                    .contains("Tool not-a-tool (call-unknown) failed: Unknown host tool")
        }));
    });
}

#[test]
fn tool_execution_failure_produces_explicit_error_result_and_tool_failure_timeline() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(ExecutionFailureToolProvider);
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "execution failure tool call");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(run_id)
                            && message
                                .content
                                .contains("execution-error:status: exit_code=9")
                    })
            },
        );
        assert!(
            settled,
            "run with execution-failure tool call did not settle"
        );

        let app = lock_unpoisoned(&app);
        assert!(app.transcript.iter().any(|message| {
            message.role == Role::Tool && message.content == "Tool bash (call-exec-failure) started"
        }));
        assert!(app.transcript.iter().any(|message| {
            message.role == Role::Tool
                && message
                    .content
                    .contains("Tool bash (call-exec-failure) failed: status: exit_code=9")
        }));
    });
}

struct MessageHistoryCaptureProvider {
    captured_messages: Arc<Mutex<Vec<Vec<RunMessage>>>>,
}

impl MessageHistoryCaptureProvider {
    fn new(captured_messages: Arc<Mutex<Vec<Vec<RunMessage>>>>) -> Self {
        Self { captured_messages }
    }
}

impl RunProvider for MessageHistoryCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        lock_unpoisoned(&self.captured_messages).push(req.messages.clone());
        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Chunk {
            run_id: req.run_id,
            text: format!("ack:{}", req.run_id),
        });
        emit(RunEvent::Finished { run_id: req.run_id });
        Ok(())
    }
}

#[test]
fn runtime_replays_model_facing_message_history_across_turns() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let captured_messages = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(MessageHistoryCaptureProvider::new(
            Arc::clone(&captured_messages),
        ));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let first_run_id = submit_prompt(&app, &mut host, "first prompt");
        let first_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(first_run_id)
                            && message.content == format!("ack:{first_run_id}")
                    })
            },
        );
        assert!(first_settled, "first run did not settle");

        let second_run_id = submit_prompt(&app, &mut host, "second prompt");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == format!("ack:{second_run_id}")
                    })
            },
        );
        assert!(second_settled, "second run did not settle");

        let captured_messages = lock_unpoisoned(&captured_messages);
        assert_eq!(captured_messages.len(), 2);
        assert_eq!(
            captured_messages[0],
            vec![RunMessage::UserText {
                text: "first prompt".to_string(),
            }]
        );
        assert_eq!(
            captured_messages[1],
            vec![
                RunMessage::UserText {
                    text: "first prompt".to_string(),
                },
                RunMessage::AssistantText {
                    text: format!("ack:{first_run_id}"),
                },
                RunMessage::UserText {
                    text: "second prompt".to_string(),
                }
            ]
        );
    });
}

struct ToolHistoryCaptureProvider {
    turn: Mutex<u32>,
    captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
}

impl ToolHistoryCaptureProvider {
    fn new(captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>) -> Self {
        Self {
            turn: Mutex::new(0),
            captured_second_turn_messages,
        }
    }
}

impl RunProvider for ToolHistoryCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let turn = {
            let mut turn = lock_unpoisoned(&self.turn);
            *turn += 1;
            *turn
        };

        emit(RunEvent::Started { run_id: req.run_id });

        match turn {
            1 => {
                emit(RunEvent::Chunk {
                    run_id: req.run_id,
                    text: "prefix ".to_string(),
                });

                let result = execute_tool(ToolCallRequest {
                    call_id: "call-memory".to_string(),
                    tool_name: "not-a-tool".to_string(),
                    arguments: json!({}),
                });
                if !result.is_error {
                    return Err(
                        "expected unknown tool call to return an explicit error".to_string()
                    );
                }

                emit(RunEvent::Chunk {
                    run_id: req.run_id,
                    text: "suffix".to_string(),
                });
            }
            2 => {
                *lock_unpoisoned(&self.captured_second_turn_messages) = Some(req.messages.clone());
                emit(RunEvent::Chunk {
                    run_id: req.run_id,
                    text: "turn-2".to_string(),
                });
            }
            _ => {
                emit(RunEvent::Chunk {
                    run_id: req.run_id,
                    text: format!("turn-{turn}"),
                });
            }
        }

        emit(RunEvent::Finished { run_id: req.run_id });
        Ok(())
    }
}

struct PostTerminalToolCallCaptureProvider {
    turn: Mutex<u32>,
    captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
    captured_post_terminal_tool_result: Arc<Mutex<Option<ToolResult>>>,
}

impl PostTerminalToolCallCaptureProvider {
    fn new(
        captured_second_turn_messages: Arc<Mutex<Option<Vec<RunMessage>>>>,
        captured_post_terminal_tool_result: Arc<Mutex<Option<ToolResult>>>,
    ) -> Self {
        Self {
            turn: Mutex::new(0),
            captured_second_turn_messages,
            captured_post_terminal_tool_result,
        }
    }
}

impl RunProvider for PostTerminalToolCallCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let turn = {
            let mut turn = lock_unpoisoned(&self.turn);
            *turn += 1;
            *turn
        };

        emit(RunEvent::Started { run_id: req.run_id });

        if turn == 1 {
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "terminal output".to_string(),
            });
            emit(RunEvent::Finished { run_id: req.run_id });

            let late_result = execute_tool(ToolCallRequest {
                call_id: "call-post-terminal".to_string(),
                tool_name: "bash".to_string(),
                arguments: json!({
                    "command": "pwd",
                    "cwd": "."
                }),
            });
            *lock_unpoisoned(&self.captured_post_terminal_tool_result) = Some(late_result);
        } else {
            *lock_unpoisoned(&self.captured_second_turn_messages) = Some(req.messages.clone());
            emit(RunEvent::Chunk {
                run_id: req.run_id,
                text: "turn-2".to_string(),
            });
            emit(RunEvent::Finished { run_id: req.run_id });
        }

        Ok(())
    }
}

#[test]
fn runtime_replays_interleaved_assistant_and_tool_history_in_exact_order() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let captured_second_turn_messages = Arc::new(Mutex::new(None));
        let provider: Arc<dyn RunProvider> = Arc::new(ToolHistoryCaptureProvider::new(Arc::clone(
            &captured_second_turn_messages,
        )));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let first_run_id = submit_prompt(&app, &mut host, "read file");
        let first_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(first_run_id)
                            && message.content == "prefix suffix"
                    })
            },
        );
        assert!(first_settled, "first tool-memory run did not settle");

        let second_run_id = submit_prompt(&app, &mut host, "what did you read?");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == "turn-2"
                    })
            },
        );
        assert!(second_settled, "second tool-memory run did not settle");

        let captured = lock_unpoisoned(&captured_second_turn_messages)
            .clone()
            .expect("second turn messages should be captured");
        assert_eq!(
            captured,
            vec![
                RunMessage::UserText {
                    text: "read file".to_string(),
                },
                RunMessage::AssistantText {
                    text: "prefix ".to_string(),
                },
                RunMessage::ToolCall {
                    call_id: "call-memory".to_string(),
                    tool_name: "not-a-tool".to_string(),
                    arguments: json!({}),
                },
                RunMessage::ToolResult {
                    call_id: "call-memory".to_string(),
                    tool_name: "not-a-tool".to_string(),
                    content: json!("Unknown host tool 'not-a-tool' for provider 'contract-test'"),
                    is_error: true,
                },
                RunMessage::AssistantText {
                    text: "suffix".to_string(),
                },
                RunMessage::UserText {
                    text: "what did you read?".to_string(),
                },
            ]
        );
    });
}

#[test]
fn post_terminal_tool_calls_are_rejected_with_stable_error_and_not_replayed() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let captured_second_turn_messages = Arc::new(Mutex::new(None));
        let captured_post_terminal_tool_result = Arc::new(Mutex::new(None));
        let provider: Arc<dyn RunProvider> = Arc::new(PostTerminalToolCallCaptureProvider::new(
            Arc::clone(&captured_second_turn_messages),
            Arc::clone(&captured_post_terminal_tool_result),
        ));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let first_run_id = submit_prompt(&app, &mut host, "first prompt");
        let first_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(first_run_id)
                            && message.content == "terminal output"
                    })
            },
        );
        assert!(first_settled, "first run did not settle");

        let late_result = lock_unpoisoned(&captured_post_terminal_tool_result)
            .clone()
            .expect("post-terminal tool result should be captured");
        assert!(late_result.is_error);
        assert_eq!(
            tool_result_content_text(&late_result),
            POST_TERMINAL_TOOL_REJECTION_ERROR
        );
        assert_eq!(
            late_result.content,
            json!(POST_TERMINAL_TOOL_REJECTION_ERROR)
        );

        let second_run_id = submit_prompt(&app, &mut host, "second prompt");
        let second_settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant
                            && message.run_id == Some(second_run_id)
                            && message.content == "turn-2"
                    })
            },
        );
        assert!(second_settled, "second run did not settle");

        let captured = lock_unpoisoned(&captured_second_turn_messages)
            .clone()
            .expect("second turn messages should be captured");
        assert_eq!(
            captured,
            vec![
                RunMessage::UserText {
                    text: "first prompt".to_string(),
                },
                RunMessage::AssistantText {
                    text: "terminal output".to_string(),
                },
                RunMessage::UserText {
                    text: "second prompt".to_string(),
                },
            ]
        );

        let app = lock_unpoisoned(&app);
        assert!(!app
            .transcript
            .iter()
            .any(|message| message.role == Role::Tool && message.run_id == Some(first_run_id)));
    });
}

struct InstructionCaptureProvider {
    captured_instructions: Arc<Mutex<Vec<String>>>,
}

impl InstructionCaptureProvider {
    fn new(captured_instructions: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            captured_instructions,
        }
    }
}

impl RunProvider for InstructionCaptureProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        lock_unpoisoned(&self.captured_instructions).push(req.instructions.clone());
        emit(RunEvent::Started { run_id: req.run_id });
        emit(RunEvent::Finished { run_id: req.run_id });
        Ok(())
    }
}

#[test]
fn runtime_composes_non_empty_instructions_with_tool_policy() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::with_system_instructions(Some(
            "Base system block".to_string(),
        ))));
        let captured_instructions = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(InstructionCaptureProvider::new(Arc::clone(
            &captured_instructions,
        )));
        let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider);

        let run_id = submit_prompt(&app, &mut host, "capture instructions");

        let settled = wait_until(
            Duration::from_secs(2),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant && message.run_id == Some(run_id)
                    })
            },
        );
        assert!(settled, "instruction capture run did not settle");

        let captured = lock_unpoisoned(&captured_instructions);
        assert_eq!(captured.len(), 1);
        let instructions = &captured[0];
        assert!(!instructions.trim().is_empty());
        assert!(instructions.contains("Base system block"));
        assert!(instructions.contains("Tool use policy"));
        assert!(instructions.contains("read"));
        assert!(instructions.contains("bash"));
        assert!(instructions.contains("edit"));
        assert!(instructions.contains("write"));
        assert!(instructions.contains("apply_patch"));
    });
}
