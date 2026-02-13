use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use coding_agent::app::{system_instructions_from_env, App};
use coding_agent::providers;
use coding_agent::runtime::RuntimeController;
use coding_agent::tui::AppComponent;
use tape_tui::{ProcessTerminal, TUI};

fn main() -> io::Result<()> {
    let system_instructions = system_instructions_from_env();
    let app = Arc::new(Mutex::new(App::with_system_instructions(Some(
        system_instructions,
    ))));

    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let runtime_handle = tui.runtime_handle();

    let provider = providers::provider_from_env().map_err(io::Error::other)?;
    let provider_profile = provider.profile();

    let host = RuntimeController::new(Arc::clone(&app), runtime_handle, provider);
    let root_component = tui.register_component(AppComponent::new(
        Arc::clone(&app),
        Arc::clone(&host),
        provider_profile,
    ));
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
