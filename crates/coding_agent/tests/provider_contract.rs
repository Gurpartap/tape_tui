use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use coding_agent::app::{App, Mode, Role, RunId};
use coding_agent::provider::{ProviderProfile, RunProvider, RunRequest};
use coding_agent::runtime::{RunEvent, RuntimeController};
use coding_agent::tools::{BuiltinToolExecutor, ToolExecutor};
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

struct LifecycleProvider;

impl RunProvider for LifecycleProvider {
    fn profile(&self) -> ProviderProfile {
        test_provider_profile()
    }

    fn run(
        &self,
        req: RunRequest,
        _cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        _tools: &mut dyn ToolExecutor,
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
        _cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        _tools: &mut dyn ToolExecutor,
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
        cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        _tools: &mut dyn ToolExecutor,
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
        _cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        _tools: &mut dyn ToolExecutor,
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
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

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
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

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
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

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
    });
}

#[test]
fn stale_run_events_are_ignored_and_do_not_corrupt_active_run_output() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(StaleEventProvider);
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

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
