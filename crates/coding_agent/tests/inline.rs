use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coding_agent::app::{App, Mode, Role};
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
    let model: Arc<dyn ModelBackend> = Arc::new(MockBackend::new(vec![
        "first chunk\n".to_string(),
        "second chunk".to_string(),
    ]));
    let tools = BuiltinToolExecutor::new(".").expect("workspace root resolves");
    let host = RuntimeController::new(Arc::clone(&app), runtime_handle, model, tools);

    let root = tui.register_component(AppComponent::new(Arc::clone(&app), host));
    tui.set_root(vec![root]);

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

#[test]
fn normal_flow_stays_inline_without_alternate_screen_sequences() {
    let (mut tui, app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "hello from inline test");
    support::inject_input(&terminal_trace, "\r");

    let run_finished = run_until(&mut tui, Duration::from_secs(3), || {
        let app = support::lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app.transcript.iter().any(|message| {
                message.role == Role::Assistant && !message.streaming && !message.content.is_empty()
            })
    });
    assert!(run_finished, "model run did not complete in time");

    support::inject_input(&terminal_trace, "/quit");
    support::inject_input(&terminal_trace, "\r");

    let exited = run_until(&mut tui, Duration::from_secs(2), || {
        support::lock_unpoisoned(&app).should_exit
    });
    assert!(exited, "quit command did not flip should_exit");

    tui.stop().expect("runtime stop");

    let output = support::rendered_output(&terminal_trace);
    for sequence in ["\x1b[?1049h", "\x1b[?1049l", "\x1b[?1047h", "\x1b[?1047l", "\x1b[?47h", "\x1b[?47l"] {
        assert!(
            !output.contains(sequence),
            "inline runtime emitted alternate-screen sequence: {sequence:?}"
        );
    }
}
