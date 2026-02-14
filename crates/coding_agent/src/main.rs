use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use coding_agent::app::{system_instructions_from_env, App};
use coding_agent::providers;
use coding_agent::runtime::RuntimeController;
use coding_agent::tui::AppComponent;
use session_store::SessionStore;
use tape_tui::{prewarm_markdown_highlighting, ProcessTerminal, TUI};

fn main() -> io::Result<()> {
    let _ = std::thread::Builder::new()
        .name("markdown-highlight-prewarm".to_string())
        .spawn(prewarm_markdown_highlighting);

    let system_instructions = system_instructions_from_env();
    let app = Arc::new(Mutex::new(App::with_system_instructions(Some(
        system_instructions,
    ))));

    let cwd = std::env::current_dir().map_err(io::Error::other)?;
    let session_store = SessionStore::create_new(&cwd).map_err(io::Error::other)?;
    let startup_session_id = session_store.header().session_id.clone();

    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let runtime_handle = tui.runtime_handle();

    let provider = providers::provider_from_env_with_session_id(Some(&startup_session_id))
        .map_err(io::Error::other)?;
    let provider_profile = provider.profile();

    let host = RuntimeController::new_with_session_store(
        Arc::clone(&app),
        runtime_handle,
        provider,
        session_store,
    );
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
