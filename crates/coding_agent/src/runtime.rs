use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use tape_tui::runtime::tui::{
    Command, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle,
};

use crate::app::{App, HostOps, Mode, RunId};
use crate::model::{ModelBackend, RunRequest};
use crate::tools::BuiltinToolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Started { run_id: RunId },
    Chunk { run_id: RunId, text: String },
    Finished { run_id: RunId },
    Failed { run_id: RunId, error: String },
    Cancelled { run_id: RunId },
}

impl RunEvent {
    fn run_id(&self) -> RunId {
        match self {
            Self::Started { run_id }
            | Self::Finished { run_id }
            | Self::Cancelled { run_id } => *run_id,
            Self::Chunk { run_id, .. } | Self::Failed { run_id, .. } => *run_id,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Failed { .. } | Self::Cancelled { .. })
    }
}

struct ActiveRun {
    run_id: RunId,
    cancel: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

pub struct RuntimeController {
    app: Arc<Mutex<App>>,
    runtime_handle: RuntimeHandle,
    pending_events: Arc<Mutex<VecDeque<RunEvent>>>,
    next_run_id: AtomicU64,
    active_run: Mutex<Option<ActiveRun>>,
    model: Arc<dyn ModelBackend>,
    tools: Mutex<BuiltinToolExecutor>,
}

impl RuntimeController {
    /// Creates a controller that buffers run events before applying them to `App`.
    ///
    /// In UI environments, events are drained by the runtime command path. In headless or
    /// non-polling environments, call [`RuntimeController::flush_pending_run_events`] after
    /// enqueuing events to ensure queued run state is applied.
    pub fn new(
        app: Arc<Mutex<App>>,
        runtime_handle: RuntimeHandle,
        model: Arc<dyn ModelBackend>,
        tools: BuiltinToolExecutor,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            runtime_handle,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            next_run_id: AtomicU64::new(1),
            active_run: Mutex::new(None),
            model,
            tools: Mutex::new(tools),
        })
    }

    fn start_run_internal(self: &Arc<Self>, prompt: String) -> Result<RunId, String> {
        let mut active_run = self.lock_active_run();
        if active_run.is_some() {
            return Err("Run already active".to_string());
        }

        let run_id = self.next_run_id.fetch_add(1, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        let request = RunRequest { run_id, prompt };
        let join_handle = self.spawn_worker(request, Arc::clone(&cancel))?;

        *active_run = Some(ActiveRun {
            run_id,
            cancel,
            join_handle: Some(join_handle),
        });

        Ok(run_id)
    }

    fn spawn_worker(
        self: &Arc<Self>,
        request: RunRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<JoinHandle<()>, String> {
        let run_id = request.run_id;
        let controller = Arc::clone(self);
        thread::Builder::new()
            .name(format!("coding-agent-run-{run_id}"))
            .spawn(move || controller.run_worker(request, cancel))
            .map_err(|error| format!("Failed to spawn run worker: {error}"))
    }

    fn run_worker(self: Arc<Self>, request: RunRequest, cancel: Arc<AtomicBool>) {
        let run_id = request.run_id;
        self.wait_for_app_run_visibility(run_id);

        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let terminal_emitted_for_emit = Arc::clone(&terminal_emitted);
        let controller = Arc::clone(&self);
        let model = Arc::clone(&self.model);
        let tools = &self.tools;

        let mut emit = move |event: RunEvent| {
            if event.is_terminal() {
                terminal_emitted_for_emit.store(true, Ordering::SeqCst);
            }

            controller.enqueue_run_event(event);
        };
        let run_outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut tools = lock_unpoisoned(tools);
            model
                .run(request, Arc::clone(&cancel), &mut emit, &mut *tools)
        }));

        match run_outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => emit(RunEvent::Failed { run_id, error }),
            Err(_) => emit(RunEvent::Failed {
                run_id,
                error: "Model backend panicked".to_string(),
            }),
        }

        if !terminal_emitted.load(Ordering::SeqCst) && self.is_active_run_id(run_id) {
            emit(RunEvent::Failed {
                run_id,
                error: "Model backend exited without terminal event".to_string(),
            });
        }
    }

    fn enqueue_run_event(self: &Arc<Self>, event: RunEvent) {
        let should_drain = {
            let mut queue = lock_unpoisoned(&self.pending_events);
            let should_drain = queue.is_empty();
            queue.push_back(event);
            should_drain
        };

        if should_drain {
            self.runtime_handle
                .dispatch(Command::Custom(Box::new(DrainRunEventsCommand {
                    controller: Arc::clone(self),
                })));
        }
    }

    fn drain_pending_run_events(&self) -> usize {
        let mut drained = 0usize;

        loop {
            let event = {
                let mut pending_events = lock_unpoisoned(&self.pending_events);
                pending_events.pop_front()
            };

            match event {
                Some(event) => {
                    self.apply_run_event(event);
                    drained += 1;
                }
                None => break,
            }
        }

        drained
    }

    /// Drains queued run events and schedules a render.
    ///
    /// Use this in non-ticking environments (for example headless test
    /// harnesses or external callers that never call `RuntimeHandle::run_once`) to
    /// guarantee queued `RunEvent`s are applied.
    pub fn flush_pending_run_events(&self) -> usize {
        let drained = self.drain_pending_run_events();
        if drained > 0 {
            self.runtime_handle.dispatch(Command::RequestRender);
        }

        drained
    }

    fn wait_for_app_run_visibility(&self, run_id: RunId) {
        for _ in 0..256 {
            let run_visible = {
                let app = lock_unpoisoned(&self.app);
                matches!(app.mode, Mode::Running { run_id: current } if current == run_id)
            };

            if run_visible {
                return;
            }

            thread::yield_now();
        }
    }

    fn apply_run_event(&self, event: RunEvent) {
        let run_id = event.run_id();
        let terminal = event.is_terminal();

        {
            let mut app = lock_unpoisoned(&self.app);
            match event {
                RunEvent::Started { run_id } => app.on_run_started(run_id),
                RunEvent::Chunk { run_id, text } => app.on_run_chunk(run_id, &text),
                RunEvent::Finished { run_id } => app.on_run_finished(run_id),
                RunEvent::Failed { run_id, error } => app.on_run_failed(run_id, &error),
                RunEvent::Cancelled { run_id } => app.on_run_cancelled(run_id),
            }
        }

        if terminal {
            self.clear_active_run_if_matching(run_id);
        }
    }

    fn clear_active_run_if_matching(&self, run_id: RunId) {
        let mut active_run = self.lock_active_run();
        let matches = active_run.as_ref().map(|active| active.run_id) == Some(run_id);
        if !matches {
            return;
        }

        let mut completed = match active_run.take() {
            Some(completed) => completed,
            None => return,
        };

        if let Some(join_handle) = completed.join_handle.take() {
            let is_current_thread = join_handle.thread().id() == thread::current().id();
            if !is_current_thread && join_handle.is_finished() {
                let _ = join_handle.join();
            }
        }
    }

    fn is_active_run_id(&self, run_id: RunId) -> bool {
        self.lock_active_run()
            .as_ref()
            .map(|active| active.run_id)
            == Some(run_id)
    }

    fn cancel_run_internal(&self, run_id: RunId) {
        let active_run = self.lock_active_run();
        if let Some(active_run) = active_run.as_ref() {
            if active_run.run_id == run_id {
                active_run.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    fn lock_active_run(&self) -> MutexGuard<'_, Option<ActiveRun>> {
        lock_unpoisoned(&self.active_run)
    }
}

struct DrainRunEventsCommand {
    controller: Arc<RuntimeController>,
}

impl CustomCommand for DrainRunEventsCommand {
    fn name(&self) -> &'static str {
        "drain_run_events"
    }

    fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
        let drained = self.controller.drain_pending_run_events();
        if drained > 0 {
            ctx.request_render();
        }
        Ok(())
    }
}

impl HostOps for Arc<RuntimeController> {
    fn start_run(&mut self, prompt: String) -> Result<RunId, String> {
        self.start_run_internal(prompt)
    }

    fn cancel_run(&mut self, run_id: RunId) {
        self.cancel_run_internal(run_id);
    }

    fn request_render(&mut self) {
        self.runtime_handle.dispatch(Command::RequestRender);
    }

    fn request_stop(&mut self) {
        self.runtime_handle.dispatch(Command::RequestStop);
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
