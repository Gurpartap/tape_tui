use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use coding_agent::app::{App, HostOps, Mode, Role, RunId};
use coding_agent::model::{ModelBackend, RunRequest};
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
struct BlockingCancelBackend;

impl ModelBackend for BlockingCancelBackend {
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
struct RacingCancelBackend;

impl ModelBackend for RacingCancelBackend {
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
struct FlushFallbackBackend;

impl ModelBackend for FlushFallbackBackend {
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
    runtime_handle: tape_tui::runtime::tui::RuntimeHandle,
    running: Arc<AtomicBool>,
    loop_handle: Option<thread::JoinHandle<()>>,
}

impl RuntimeLoopHandle {
    fn new() -> Self {
        let mut runtime = TUI::new(NullTerminal);
        runtime.start().expect("runtime start");

        let runtime_handle = runtime.runtime_handle();
        let running = Arc::new(AtomicBool::new(true));
        let loop_running = Arc::clone(&running);

        let loop_handle = thread::spawn(move || {
            while loop_running.load(Ordering::SeqCst) {
                runtime.run_once();
                thread::sleep(Duration::from_millis(5));
            }

            let _ = runtime.stop();
        });

        Self {
            runtime_handle,
            running,
            loop_handle: Some(loop_handle),
        }
    }

    fn runtime_handle(&self) -> tape_tui::runtime::tui::RuntimeHandle {
        self.runtime_handle.clone()
    }
}

impl Drop for RuntimeLoopHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(loop_handle) = self.loop_handle.take() {
            let _ = loop_handle.join();
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return true;
        }

        thread::sleep(Duration::from_millis(10));
    }

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
    let app = Arc::new(Mutex::new(App::new()));
    let model: Arc<dyn ModelBackend> = Arc::new(BlockingCancelBackend);
    let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
    let runtime_loop = RuntimeLoopHandle::new();
    let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), model, tools);

    {
        let mut app = lock_unpoisoned(&app);
        app.on_input_replace("long running task".to_string());
        app.on_submit(&mut host);
        assert!(matches!(app.mode, Mode::Running { .. }));
        app.on_cancel(&mut host);
    }

    let finished = wait_until(Duration::from_secs(3), || {
        let app = lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::System && message.content == "Run cancelled")
    });

    if !finished {
        let app = lock_unpoisoned(&app);
        panic!("run did not settle to cancelled idle state: {:?}", app.mode);
    }

    let app = lock_unpoisoned(&app);
    assert_eq!(app.mode, Mode::Idle);
    assert!(app.transcript.iter().any(|message| {
        message.role == Role::Assistant && message.content == "working..." && !message.streaming
    }));
}

#[test]
fn repeated_cancel_is_a_noop_after_first_signal() {
    let app = Arc::new(Mutex::new(App::new()));
    let model: Arc<dyn ModelBackend> = Arc::new(BlockingCancelBackend);
    let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
    let runtime_loop = RuntimeLoopHandle::new();
    let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), model, tools);

    let run_id = {
        let mut app = lock_unpoisoned(&app);
        app.on_input_replace("task to cancel repeatedly".to_string());
        app.on_submit(&mut host);
        running_run_id(&app.mode)
    };

    host.cancel_run(run_id);
    host.cancel_run(run_id);

    let finished = wait_until(Duration::from_secs(3), || {
        let app = lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
    });
    assert!(finished, "run did not settle after repeated cancel calls");

    let cancelled_count_after_first_completion = {
        let app = lock_unpoisoned(&app);
        app.transcript
            .iter()
            .filter(|message| message.role == Role::System && message.content == "Run cancelled")
            .count()
    };
    assert_eq!(cancelled_count_after_first_completion, 1);

    host.cancel_run(run_id);
    thread::sleep(Duration::from_millis(25));

    let cancelled_count_after_extra_cancel = {
        let app = lock_unpoisoned(&app);
        app.transcript
            .iter()
            .filter(|message| message.role == Role::System && message.content == "Run cancelled")
            .count()
    };

    assert_eq!(cancelled_count_after_extra_cancel, 1);
}

#[test]
fn cancel_race_merges_chunks_into_single_assistant_message() {
    let app = Arc::new(Mutex::new(App::new()));
    let model: Arc<dyn ModelBackend> = Arc::new(RacingCancelBackend);
    let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");
    let runtime_loop = RuntimeLoopHandle::new();
    let mut host = RuntimeController::new(app.clone(), runtime_loop.runtime_handle(), model, tools);

    let run_id = {
        let mut app = lock_unpoisoned(&app);
        app.on_input_replace("cancel race".to_string());
        app.on_submit(&mut host);
        running_run_id(&app.mode)
    };

    // Try to cancel in the middle of streaming output.
    let streaming_started = wait_until(Duration::from_secs(1), || {
        let app = lock_unpoisoned(&app);
        app.transcript
            .iter()
            .any(|message| {
                message.role == Role::Assistant
                    && message.run_id == Some(run_id)
                    && message.content.contains("first")
            })
    });
    assert!(streaming_started, "run did not start streaming before cancellation");

    host.cancel_run(run_id);

    let settled = wait_until(Duration::from_secs(3), || {
        let app = lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::System && message.content == "Run cancelled")
    });
    assert!(settled, "cancel race did not settle");

    let app = lock_unpoisoned(&app);
    let assistant_messages: Vec<_> = app
        .transcript
        .iter()
        .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
        .collect();

    assert_eq!(assistant_messages.len(), 1);
    assert_eq!(assistant_messages[0].content, "first second");
    assert!(!assistant_messages[0].streaming);
}

#[test]
fn flush_pending_events_in_headless_usage() {
    let app = Arc::new(Mutex::new(App::new()));
    let model: Arc<dyn ModelBackend> = Arc::new(FlushFallbackBackend);
    let tools = BuiltinToolExecutor::new(".").expect("workspace root must resolve");

    // This test intentionally avoids running the TUI loop to emulate a headless caller
    // that must drive event application explicitly via flush_pending_run_events.
    let mut runtime = TUI::new(NullTerminal);
    runtime.start().expect("runtime start");
    let runtime_handle = runtime.runtime_handle();
    let mut host = RuntimeController::new(app.clone(), runtime_handle, model, tools);

    let run_id = {
        let mut app = lock_unpoisoned(&app);
        app.on_input_replace("flush fallback".to_string());
        app.on_submit(&mut host);
        match app.mode {
            Mode::Running { run_id } => run_id,
            _ => unreachable!(),
        }
    };

    let before_flush = wait_until(Duration::from_millis(100), || {
        let app = lock_unpoisoned(&app);
        app.transcript
            .iter()
            .all(|message| message.role != Role::Assistant || message.content != "deferred flush")
    });
    assert!(before_flush, "assistant content should remain unmerged before draining");

    let drained = host.flush_pending_run_events();
    assert!(drained >= 4, "expected at least 4 queued run events, got {drained}");

    let settled = wait_until(Duration::from_secs(1), || {
        let app = lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
    });
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

    runtime.stop().expect("runtime stop");
}
