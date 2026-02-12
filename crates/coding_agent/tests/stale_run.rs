use coding_agent::app::{App, HostOps, Message, Mode, Role, RunId};

struct HostStub {
    next_run_id: RunId,
}

impl HostStub {
    fn new(next_run_id: RunId) -> Self {
        Self { next_run_id }
    }
}

impl HostOps for HostStub {
    fn start_run(&mut self, _prompt: String) -> Result<RunId, String> {
        Ok(self.next_run_id)
    }

    fn cancel_run(&mut self, _run_id: RunId) {}

    fn request_render(&mut self) {}

    fn request_stop(&mut self) {}
}

#[test]
fn stale_run_callbacks_are_ignored_while_different_run_is_active() {
    let stale_run = 10;
    let active_run = 20;

    let mut app = App::new();
    let mut host = HostStub::new(active_run);

    app.on_input_replace("active prompt".to_string());
    app.on_submit(&mut host);
    app.on_run_started(active_run);
    app.on_run_chunk(active_run, "live output");

    let snapshot_mode = app.mode.clone();
    let snapshot_transcript = app.transcript.clone();

    app.on_run_started(stale_run);
    app.on_run_chunk(stale_run, "stale chunk");
    app.on_run_finished(stale_run);
    app.on_run_failed(stale_run, "stale error");
    app.on_run_cancelled(stale_run);

    assert_eq!(app.mode, snapshot_mode);
    assert_eq!(app.transcript, snapshot_transcript);

    app.on_run_chunk(active_run, " + still live");
    assert_eq!(app.mode, Mode::Running { run_id: active_run });
    assert_eq!(
        app.transcript.last(),
        Some(&Message {
            role: Role::Assistant,
            content: "live output + still live".to_string(),
            streaming: true,
            run_id: Some(active_run),
        })
    );
}
