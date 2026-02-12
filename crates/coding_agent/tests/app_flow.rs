use coding_agent::app::{App, HostOps, Message, Mode, Role, RunId};
use coding_agent::commands::{parse_slash_command, SlashCommand};

#[derive(Default)]
struct HostSpy {
    next_run_id: RunId,
    started_prompts: Vec<String>,
    cancelled_runs: Vec<RunId>,
    render_requests: usize,
    stop_requests: usize,
}

impl HostSpy {
    fn with_next_run_id(run_id: RunId) -> Self {
        Self {
            next_run_id: run_id,
            ..Self::default()
        }
    }
}

impl HostOps for HostSpy {
    fn start_run(&mut self, prompt: String) -> Result<RunId, String> {
        self.started_prompts.push(prompt);
        Ok(self.next_run_id)
    }

    fn cancel_run(&mut self, run_id: RunId) {
        self.cancelled_runs.push(run_id);
    }

    fn request_render(&mut self) {
        self.render_requests += 1;
    }

    fn request_stop(&mut self) {
        self.stop_requests += 1;
    }
}

#[test]
fn submit_starts_run_and_enters_running_mode() {
    let mut app = App::new();
    let mut host = HostSpy::with_next_run_id(42);

    app.on_input_replace("describe the module layout".to_string());
    app.on_submit(&mut host);

    assert_eq!(host.started_prompts, vec!["describe the module layout".to_string()]);
    assert_eq!(app.history.entries, vec!["describe the module layout".to_string()]);
    assert_eq!(app.mode, Mode::Running { run_id: 42 });
    assert_eq!(app.input, "");
    assert_eq!(app.transcript.len(), 1);
    assert_eq!(
        app.transcript[0],
        Message {
            role: Role::User,
            content: "describe the module layout".to_string(),
            streaming: false,
            run_id: None,
        }
    );
    assert_eq!(host.render_requests, 1);
}

#[test]
fn parser_recognizes_known_and_unknown_slash_commands() {
    assert_eq!(parse_slash_command("plain prompt"), None);
    assert_eq!(parse_slash_command("/help"), Some(SlashCommand::Help));
    assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
    assert_eq!(parse_slash_command("/cancel"), Some(SlashCommand::Cancel));
    assert_eq!(parse_slash_command("/quit"), Some(SlashCommand::Quit));
    assert_eq!(
        parse_slash_command("/nope extra args"),
        Some(SlashCommand::Unknown("/nope".to_string()))
    );
}

#[test]
fn slash_help_clear_and_quit_have_expected_semantics() {
    let mut app = App::new();
    let mut host = HostSpy::default();

    app.on_input_replace("/help".to_string());
    app.on_submit(&mut host);

    assert_eq!(app.mode, Mode::Idle);
    assert_eq!(host.started_prompts.len(), 0);
    assert!(app
        .transcript
        .last()
        .expect("help message exists")
        .content
        .contains("/help"));

    app.transcript.push(Message {
        role: Role::Assistant,
        content: "leftover".to_string(),
        streaming: false,
        run_id: None,
    });

    app.on_input_replace("/clear".to_string());
    app.on_submit(&mut host);

    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].role, Role::System);
    assert_eq!(app.transcript[0].content, "Transcript cleared");

    app.on_input_replace("/quit".to_string());
    app.on_submit(&mut host);

    assert_eq!(app.mode, Mode::Exiting);
    assert!(app.should_exit);
    assert_eq!(host.stop_requests, 1);
}

#[test]
fn slash_cancel_cancels_active_run_or_reports_idle_state() {
    let mut app = App::new();
    let mut host = HostSpy::with_next_run_id(7);

    app.on_input_replace("/cancel".to_string());
    app.on_submit(&mut host);
    assert_eq!(host.cancelled_runs, Vec::<RunId>::new());
    assert_eq!(
        app.transcript.last().expect("idle cancel message").content,
        "No active run"
    );

    app.on_input_replace("run something".to_string());
    app.on_submit(&mut host);
    assert_eq!(app.mode, Mode::Running { run_id: 7 });

    app.on_input_replace("/cancel".to_string());
    app.on_submit(&mut host);
    assert_eq!(host.cancelled_runs, vec![7]);
}

#[test]
fn sending_message_while_running_is_non_failing() {
    let mut app = App::new();
    let mut host = HostSpy::with_next_run_id(11);

    app.on_input_replace("run while running".to_string());
    app.on_submit(&mut host);
    assert_eq!(app.mode, Mode::Running { run_id: 11 });
    assert_eq!(host.started_prompts.len(), 1);

    app.on_input_replace("another message".to_string());
    app.on_submit(&mut host);

    assert_eq!(
        app.transcript
            .last()
            .expect("system message exists")
            .content,
        "Run already in progress. Use /cancel to stop it."
    );
    assert_eq!(app.mode, Mode::Running { run_id: 11 });
    assert_eq!(host.started_prompts.len(), 1);
    assert_eq!(host.render_requests, 2);
}
