use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coding_agent::app::{App, Mode};
use coding_agent::model::{MockBackend, ModelBackend};
use coding_agent::runtime::RuntimeController;
use coding_agent::tools::BuiltinToolExecutor;
use coding_agent::tui::AppComponent;
use tape_tui::TUI;

mod support;

fn setup_runtime() -> (TUI<support::SharedTerminal>, Arc<Mutex<App>>, Arc<Mutex<support::TerminalTrace>>) {
    let app = Arc::new(Mutex::new(App::new()));
    let (terminal, terminal_trace) = support::SharedTerminal::new(120, 40);
    let mut tui = TUI::new(terminal);

    let runtime_handle = tui.runtime_handle();
    let model: Arc<dyn ModelBackend> = Arc::new(MockBackend::default());
    let tools = BuiltinToolExecutor::new(".").expect("workspace root resolves");
    let host = RuntimeController::new(Arc::clone(&app), runtime_handle, model, tools);

    let root = tui.register_component(AppComponent::new(Arc::clone(&app), host));
    tui.set_root(vec![root]);
    tui.set_focus(root);

    (tui, app, terminal_trace)
}

fn run_until(
    tui: &mut TUI<support::SharedTerminal>,
    timeout: Duration,
    mut predicate: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }

        tui.run_once();
        thread::sleep(Duration::from_millis(5));
    }

    predicate()
}

fn assert_teardown_sequences_and_counts(trace: &Arc<Mutex<support::TerminalTrace>>) {
    let output = support::rendered_output(trace);
    assert!(
        output.contains("\x1b[?25h"),
        "show-cursor escape was not emitted during teardown"
    );
    assert!(
        output.contains("\x1b[?2004l"),
        "bracketed-paste disable escape was not emitted during teardown"
    );

    let trace = support::lock_unpoisoned(trace);
    assert_eq!(trace.start_calls, 1, "runtime should start once");
    assert_eq!(trace.stop_calls, 1, "runtime should stop once");
    assert_eq!(
        trace.drain_calls.len(),
        1,
        "runtime should drain input exactly once"
    );
}

#[test]
fn quit_command_path_restores_terminal_state() {
    let (mut tui, app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "/quit");
    support::inject_input(&terminal_trace, "\r");

    let exited = run_until(&mut tui, Duration::from_secs(2), || {
        support::lock_unpoisoned(&app).should_exit
    });
    assert!(exited, "quit command did not trigger exit state");
    assert!(matches!(support::lock_unpoisoned(&app).mode, Mode::Exiting));

    tui.stop().expect("runtime stop");
    assert_teardown_sequences_and_counts(&terminal_trace);
}

#[test]
fn ctrl_c_path_restores_terminal_state() {
    let (mut tui, app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "\x03");

    let exited = run_until(&mut tui, Duration::from_secs(2), || {
        support::lock_unpoisoned(&app).should_exit
    });
    assert!(exited, "ctrl+c did not trigger exit state");
    assert!(matches!(support::lock_unpoisoned(&app).mode, Mode::Exiting));

    tui.stop().expect("runtime stop");
    assert_teardown_sequences_and_counts(&terminal_trace);
}
