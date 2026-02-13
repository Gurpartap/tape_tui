use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use coding_agent::app::App;
use coding_agent::model::{MockBackend, ModelBackend};
use coding_agent::runtime::RuntimeController;
use coding_agent::tools::BuiltinToolExecutor;
use coding_agent::tui::AppComponent;
use tape_tui::{ProcessTerminal, TUI};

fn main() -> io::Result<()> {
    let app = Arc::new(Mutex::new(App::new()));

    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let runtime_handle = tui.runtime_handle();

    let model: Arc<dyn ModelBackend> = Arc::new(MockBackend::default());
    let workspace_root = std::env::current_dir()?;
    let tools = BuiltinToolExecutor::new(workspace_root)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;

    let host = RuntimeController::new(Arc::clone(&app), runtime_handle, model, tools);
    let root_component =
        tui.register_component(AppComponent::new(Arc::clone(&app), Arc::clone(&host)));
    tui.set_root(vec![root_component]);
    tui.set_focus(root_component);

    tui.start()?;

    while !lock_unpoisoned(&app).should_exit {
        tui.run_blocking_once();
    }

    tui.stop()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
