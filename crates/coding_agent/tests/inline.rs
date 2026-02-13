use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coding_agent::app::{App, Mode, Role};
use coding_agent::provider::{RunProvider, RunRequest};
use coding_agent::providers::MockProvider;
use coding_agent::runtime::{RunEvent, RuntimeController};
use coding_agent::tools::{BuiltinToolExecutor, ToolExecutor};
use coding_agent::tui::AppComponent;
use tape_tui::TUI;

mod support;

#[derive(Default)]
struct BlockingProvider;

impl RunProvider for BlockingProvider {
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
struct OrderedChunkProvider;

impl RunProvider for OrderedChunkProvider {
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
            text: "hello ".to_string(),
        });
        emit(RunEvent::Chunk {
            run_id,
            text: "world".to_string(),
        });
        emit(RunEvent::Finished { run_id });

        Ok(())
    }
}

fn setup_runtime_with_provider(
    provider: Arc<dyn RunProvider>,
) -> (
    TUI<support::SharedTerminal>,
    Arc<Mutex<App>>,
    Arc<Mutex<support::TerminalTrace>>,
) {
    let app = Arc::new(Mutex::new(App::new()));
    let (terminal, terminal_trace) = support::SharedTerminal::new(120, 40);
    let mut tui = TUI::new(terminal);

    let runtime_handle = tui.runtime_handle();
    let tools = BuiltinToolExecutor::new(".").expect("workspace root resolves");
    let host = RuntimeController::new(Arc::clone(&app), runtime_handle, provider, tools);

    let root = tui.register_component(AppComponent::new(Arc::clone(&app), host));
    tui.set_root(vec![root]);
    tui.set_focus(root);

    (tui, app, terminal_trace)
}

fn setup_runtime() -> (
    TUI<support::SharedTerminal>,
    Arc<Mutex<App>>,
    Arc<Mutex<support::TerminalTrace>>,
) {
    setup_runtime_with_provider(Arc::new(MockProvider::new(vec![
        "first chunk\n".to_string(),
        "second chunk".to_string(),
    ])))
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
fn prompt_is_visible_on_start() {
    let (mut tui, _app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");

    let rendered = run_until(&mut tui, Duration::from_secs(1), || {
        let output = support::rendered_output(&terminal_trace);
        output.contains("Coding Agent") && output.contains("Ready") && output.contains("\x1b[7m")
    });
    assert!(rendered, "initial prompt render was not observed");

    tui.stop().expect("runtime stop");
}

#[test]
fn composer_remains_interactive_during_streaming() {
    let (mut tui, app, terminal_trace) = setup_runtime_with_provider(Arc::new(BlockingProvider));

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "streaming composer check");
    support::inject_input(&terminal_trace, "\r");

    let started = run_until(&mut tui, Duration::from_secs(2), || {
        matches!(support::lock_unpoisoned(&app).mode, Mode::Running { .. })
    });
    assert!(started, "run did not enter running mode");

    support::inject_input(&terminal_trace, " typing while running");

    let interactive = run_until(&mut tui, Duration::from_secs(2), || {
        support::rendered_output(&terminal_trace).contains("typing while running")
    });
    assert!(
        interactive,
        "composer was not interactive while run remained active"
    );

    support::inject_input(&terminal_trace, "\x1b");
    let cancelled = run_until(&mut tui, Duration::from_secs(2), || {
        let app = support::lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::System && message.content == "Run cancelled")
    });
    assert!(cancelled, "cancel did not end run during streaming");

    tui.stop().expect("runtime stop");
}

#[test]
fn escape_key_cancels_active_run() {
    let (mut tui, app, terminal_trace) = setup_runtime_with_provider(Arc::new(BlockingProvider));

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "long run");
    support::inject_input(&terminal_trace, "\r");

    let started = run_until(&mut tui, Duration::from_secs(2), || {
        matches!(support::lock_unpoisoned(&app).mode, Mode::Running { .. })
    });
    assert!(started, "run did not enter running mode");

    support::inject_input(&terminal_trace, "\x1b");

    let cancelled = run_until(&mut tui, Duration::from_secs(3), || {
        let app = support::lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::System && message.content == "Run cancelled")
    });
    assert!(cancelled, "escape key did not cancel active run");

    tui.stop().expect("runtime stop");
}

#[test]
fn escape_key_stops_streaming_immediately() {
    let (mut tui, app, terminal_trace) = setup_runtime_with_provider(Arc::new(BlockingProvider));

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "long run");
    support::inject_input(&terminal_trace, "\r");

    let started = run_until(&mut tui, Duration::from_secs(2), || {
        matches!(support::lock_unpoisoned(&app).mode, Mode::Running { .. })
    });
    assert!(started, "run did not enter running mode");

    support::inject_input(&terminal_trace, "\x1b");

    let stopped = run_until(&mut tui, Duration::from_secs(2), || {
        let app = support::lock_unpoisoned(&app);
        app.transcript.iter().any(|message| {
            message.role == Role::Assistant && message.content == "working..." && !message.streaming
        })
    });
    assert!(
        stopped,
        "escape key did not immediately stop streaming assistant text"
    );

    tui.stop().expect("runtime stop");
}

#[test]
fn run_event_queue_applies_in_order() {
    let (mut tui, app, terminal_trace) =
        setup_runtime_with_provider(Arc::new(OrderedChunkProvider));

    tui.start().expect("runtime start");
    tui.run_once();

    support::inject_input(&terminal_trace, "queue ordering");
    support::inject_input(&terminal_trace, "\r");

    let completed = run_until(&mut tui, Duration::from_secs(2), || {
        let app = support::lock_unpoisoned(&app);
        matches!(app.mode, Mode::Idle)
            && app
                .transcript
                .iter()
                .any(|message| message.role == Role::Assistant && !message.streaming)
    });
    assert!(
        completed,
        "ordered queue events did not reach a completed assistant message"
    );

    let app = support::lock_unpoisoned(&app);
    let assistant_messages: Vec<_> = app
        .transcript
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .collect();

    assert_eq!(assistant_messages.len(), 1);
    assert_eq!(assistant_messages[0].content, "hello world");
    assert!(!assistant_messages[0].streaming);

    tui.stop().expect("runtime stop");
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
    assert!(run_finished, "provider run did not complete in time");

    support::inject_input(&terminal_trace, "/quit");
    support::inject_input(&terminal_trace, "\r");

    let exited = run_until(&mut tui, Duration::from_secs(2), || {
        support::lock_unpoisoned(&app).should_exit
    });
    assert!(exited, "quit command did not flip should_exit");

    tui.stop().expect("runtime stop");

    let output = support::rendered_output(&terminal_trace);
    for sequence in [
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?1047h",
        "\x1b[?1047l",
        "\x1b[?47h",
        "\x1b[?47l",
    ] {
        assert!(
            !output.contains(sequence),
            "inline runtime emitted alternate-screen sequence: {sequence:?}"
        );
    }
}

#[test]
fn input_history_up_down_keys_cycle_and_return_to_live_draft() {
    let (mut tui, app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");

    {
        let mut app = support::lock_unpoisoned(&app);
        app.push_history_entry("first command");
        app.push_history_entry("second command");
        app.push_history_entry("third command");
        app.input = "draft input".to_string();
    }

    support::inject_input(&terminal_trace, "\x1bOA");
    let recalled_last = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "third command"
    });
    assert!(recalled_last, "up did not recall the most recent command");

    support::inject_input(&terminal_trace, "\x1b[A");
    let recalled_second = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "second command"
    });
    assert!(recalled_second, "second up did not recall previous command");

    support::inject_input(&terminal_trace, "\x1b[A");
    let recalled_third = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "first command"
    });
    assert!(
        recalled_third,
        "additional up did not stay at oldest command"
    );

    support::inject_input(&terminal_trace, "\x1bOB");
    let restored_middle = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "second command"
    });
    assert!(
        restored_middle,
        "down did not move forward through command history"
    );

    support::inject_input(&terminal_trace, "\x1b[B");
    let restored_draft = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "third command"
    });
    assert!(
        restored_draft,
        "down did not move from middle to newest command"
    );

    support::inject_input(&terminal_trace, "\x1b[B");
    let returned_to_draft = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "draft input"
    });
    assert!(
        returned_to_draft,
        "down did not return to the live draft after history"
    );

    tui.stop().expect("runtime stop");
}

#[test]
fn ctrl_c_clears_text_input_and_does_not_exit() {
    let (mut tui, app, terminal_trace) = setup_runtime();

    tui.start().expect("runtime start");

    support::inject_input(&terminal_trace, "draft input");
    let started_editing = run_until(&mut tui, Duration::from_secs(1), || {
        support::lock_unpoisoned(&app).input == "draft input"
    });
    assert!(started_editing, "typed input did not reach app state");

    support::inject_input(&terminal_trace, "\x03");
    let cleared = run_until(&mut tui, Duration::from_secs(1), || {
        let app = support::lock_unpoisoned(&app);
        app.input.is_empty() && !app.should_exit
    });
    assert!(cleared, "ctrl+c did not clear input without exiting");

    tui.stop().expect("runtime stop");
}
