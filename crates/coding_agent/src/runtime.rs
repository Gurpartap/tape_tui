use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tape_tui::runtime::tui::RuntimeHandle;

use crate::app::{App, RunId};
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
    next_run_id: std::sync::atomic::AtomicU64,
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
            next_run_id: std::sync::atomic::AtomicU64::new(1),
            active_run: Mutex::new(None),
            model,
            tools: Mutex::new(tools),
        })
    }
}
