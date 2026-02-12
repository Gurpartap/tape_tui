use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use tape_tui::runtime::tui::{Command, RuntimeHandle};

use crate::app::{App, HostOps, RunId};
use crate::model::ModelBackend;
use crate::tools::BuiltinToolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Started { run_id: RunId },
    Chunk { run_id: RunId, text: String },
    Finished { run_id: RunId },
    Failed { run_id: RunId, error: String },
    Cancelled { run_id: RunId },
}

struct ActiveRun {
    run_id: RunId,
    cancel: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

pub struct RuntimeController {
    app: Arc<Mutex<App>>,
    runtime_handle: RuntimeHandle,
    next_run_id: AtomicU64,
    active_run: Mutex<Option<ActiveRun>>,
    model: Arc<dyn ModelBackend>,
    tools: Mutex<BuiltinToolExecutor>,
}

impl RuntimeController {
    pub fn new(
        app: Arc<Mutex<App>>,
        runtime_handle: RuntimeHandle,
        model: Arc<dyn ModelBackend>,
        tools: BuiltinToolExecutor,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            runtime_handle,
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
        let join_handle = self.spawn_worker_placeholder(run_id, prompt, Arc::clone(&cancel))?;

        *active_run = Some(ActiveRun {
            run_id,
            cancel,
            join_handle: Some(join_handle),
        });

        Ok(run_id)
    }

    fn spawn_worker_placeholder(
        self: &Arc<Self>,
        run_id: RunId,
        prompt: String,
        cancel: Arc<AtomicBool>,
    ) -> Result<JoinHandle<()>, String> {
        let controller = Arc::clone(self);
        thread::Builder::new()
            .name(format!("coding-agent-run-{run_id}"))
            .spawn(move || controller.run_worker_placeholder(run_id, prompt, cancel))
            .map_err(|error| format!("Failed to spawn run worker: {error}"))
    }

    fn run_worker_placeholder(self: Arc<Self>, _run_id: RunId, _prompt: String, _cancel: Arc<AtomicBool>) {
        let _ = self;
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
