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

#[derive(Default)]
struct BlockingCancelProvider;

fn test_provider_profile() -> ProviderProfile {
    ProviderProfile {
        provider_id: "test".to_string(),
        model_id: "test-model".to_string(),
        thinking_label: Some("test-thinking".to_string()),
    }
}

impl RunProvider for BlockingCancelProvider {
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
        let run_id = req.run_id;

        emit(RunEvent::Started { run_id });
        emit(RunEvent::Chunk {
            run_id,
            text: "working...".to_string(),
        });

        while !cancel.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }

        emit(RunEvent::Cancelled { run_id });
        Ok(())
    }
}

#[derive(Default)]
struct RacingCancelProvider;

impl RunProvider for RacingCancelProvider {
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
        let run_id = req.run_id;

        emit(RunEvent::Started { run_id });
        emit(RunEvent::Chunk {
            run_id,
            text: "first".to_string(),
        });
        thread::sleep(Duration::from_millis(3));
        emit(RunEvent::Chunk {
            run_id,
            text: " second".to_string(),
        });

        while !cancel.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }

        emit(RunEvent::Cancelled { run_id });
        Ok(())
    }
}

#[derive(Default)]
struct FlushFallbackProvider;

impl RunProvider for FlushFallbackProvider {
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
        let run_id = req.run_id;

        emit(RunEvent::Started { run_id });
        emit(RunEvent::Chunk {
            run_id,
            text: "deferred ".to_string(),
        });
        emit(RunEvent::Chunk {
            run_id,
            text: "flush".to_string(),
        });
        emit(RunEvent::Finished { run_id });

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
        runtime.start().expect("runtime start");

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
        Err(panic_payload) => resume_unwind(panic_payload),
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

fn running_run_id(mode: &Mode) -> RunId {
    if let Mode::Running { run_id } = mode {
        *run_id
    } else {
        panic!("expected running mode, got {mode:?}");
    }
}

#[test]
fn cancel_while_running_results_in_cancelled_state() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(BlockingCancelProvider);
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

        let run_id = {
            let mut app = lock_unpoisoned(&app);
            app.on_input_replace("long running task".to_string());
            app.on_submit(&mut host);
            assert!(matches!(app.mode, Mode::Running { .. }));
            let run_id = running_run_id(&app.mode);
            app.on_cancel(&mut host);
            run_id
        };

        let finished = wait_until(
            Duration::from_secs(3),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::System && message.content == "Run cancelled"
                    })
                    && app.transcript.iter().any(|message| {
                        message.role == Role::Assistant && message.run_id == Some(run_id)
                    })
            },
        );

        if !finished {
            let app = lock_unpoisoned(&app);
            panic!("run did not settle to cancelled idle state: {:?}", app.mode);
        }

        let app = lock_unpoisoned(&app);
        assert_eq!(app.mode, Mode::Idle);
        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "working...");
        assert!(!assistant_messages[0].streaming);
    });
}

#[test]
fn repeated_cancel_is_a_noop_after_first_signal() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(BlockingCancelProvider);
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

        let _run_id = {
            let mut app = lock_unpoisoned(&app);
            app.on_input_replace("task to cancel repeatedly".to_string());
            app.on_submit(&mut host);
            running_run_id(&app.mode)
        };

        {
            let mut app = lock_unpoisoned(&app);
            app.on_cancel(&mut host);
            app.on_cancel(&mut host);
        }

        let finished = wait_until(
            Duration::from_secs(3),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
            },
        );
        assert!(finished, "run did not settle after repeated cancel calls");

        let cancelled_count_after_first_completion = {
            let app = lock_unpoisoned(&app);
            app.transcript
                .iter()
                .filter(|message| {
                    message.role == Role::System && message.content == "Run cancelled"
                })
                .count()
        };
        assert_eq!(cancelled_count_after_first_completion, 1);

        {
            let mut app = lock_unpoisoned(&app);
            app.on_cancel(&mut host);
        }
        thread::sleep(Duration::from_millis(25));

        let cancelled_count_after_extra_cancel = {
            let app = lock_unpoisoned(&app);
            app.transcript
                .iter()
                .filter(|message| {
                    message.role == Role::System && message.content == "Run cancelled"
                })
                .count()
        };

        assert_eq!(cancelled_count_after_extra_cancel, 1);
    });
}

#[test]
fn cancel_race_keeps_single_non_streaming_assistant_message() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(RacingCancelProvider);
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
        let mut host =
            RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), provider, tools);

        let run_id = {
            let mut app = lock_unpoisoned(&app);
            app.on_input_replace("cancel race".to_string());
            app.on_submit(&mut host);
            running_run_id(&app.mode)
        };

        // Try to cancel in the middle of streaming output.
        let streaming_started = wait_until(
            Duration::from_secs(1),
            || {
                runtime_loop.tick();
                host.flush_pending_run_events();
            },
            || {
                let app = lock_unpoisoned(&app);
                app.transcript.iter().any(|message| {
                    message.role == Role::Assistant
                        && message.run_id == Some(run_id)
                        && message.content.contains("first")
                })
            },
        );
        assert!(
            streaming_started,
            "run did not start streaming before cancellation"
        );

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
                let app = lock_unpoisoned(&app);
                matches!(app.mode, Mode::Idle)
                    && app.transcript.iter().any(|message| {
                        message.role == Role::System && message.content == "Run cancelled"
                    })
            },
        );
        assert!(settled, "cancel race did not settle");

        let app = lock_unpoisoned(&app);
        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();

        assert_eq!(assistant_messages.len(), 1);
        assert!(
            assistant_messages[0].content == "first"
                || assistant_messages[0].content == "first second"
        );
        assert!(!assistant_messages[0].streaming);
    });
}

#[test]
fn flush_pending_events_in_headless_usage() {
    with_runtime_loop(|runtime_loop| {
        let app = Arc::new(Mutex::new(App::new()));
        let provider: Arc<dyn RunProvider> = Arc::new(FlushFallbackProvider);
        let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");

        // This test intentionally avoids running the TUI loop to emulate a headless caller
        // that must drive event application explicitly via flush_pending_run_events.
        let runtime_handle = runtime_loop.runtime_handle();
        let mut host = RuntimeController::new(app.clone(), runtime_handle, provider, tools);

        let run_id = {
            let mut app = lock_unpoisoned(&app);
            app.on_input_replace("flush fallback".to_string());
            app.on_submit(&mut host);
            match app.mode {
                Mode::Running { run_id } => run_id,
                _ => unreachable!(),
            }
        };

        let before_flush = wait_until(
            Duration::from_millis(100),
            || {},
            || {
                let app = lock_unpoisoned(&app);
                app.transcript.iter().all(|message| {
                    message.role != Role::Assistant || message.content != "deferred flush"
                })
            },
        );
        assert!(
            before_flush,
            "assistant content should remain unmerged before draining"
        );

        let settled = wait_until(
            Duration::from_secs(1),
            || {
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
        assert!(settled, "headless flush did not apply queued events");

        let app = lock_unpoisoned(&app);
        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();

        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "deferred flush");
        assert!(!assistant_messages[0].streaming);
    });
}
