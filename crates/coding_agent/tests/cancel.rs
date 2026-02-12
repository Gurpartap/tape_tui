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

fn runtime_handle() -> tape_tui::runtime::tui::RuntimeHandle {
    let runtime = TUI::new(NullTerminal);
    runtime.runtime_handle()
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
    let mut host = RuntimeController::new(app.clone(), runtime_handle(), model, tools);

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
    let mut host = RuntimeController::new(app.clone(), runtime_handle(), model, tools);

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
