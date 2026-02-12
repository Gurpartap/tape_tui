use std::sync::{Arc, Mutex, MutexGuard};

use tape_tui::Terminal;

type InputHandler = Box<dyn FnMut(String) + Send>;
type ResizeHandler = Box<dyn FnMut() + Send>;

#[derive(Default)]
pub struct TerminalTrace {
    pub writes: Vec<String>,
    pub start_calls: usize,
    pub stop_calls: usize,
    pub drain_calls: Vec<(u64, u64)>,
    pub on_input: Option<InputHandler>,
    pub on_resize: Option<ResizeHandler>,
}

pub struct SharedTerminal {
    state: Arc<Mutex<TerminalTrace>>,
    columns: u16,
    rows: u16,
}

impl SharedTerminal {
    pub fn new(columns: u16, rows: u16) -> (Self, Arc<Mutex<TerminalTrace>>) {
        let state = Arc::new(Mutex::new(TerminalTrace::default()));
        (
            Self {
                state: Arc::clone(&state),
                columns,
                rows,
            },
            state,
        )
    }
}

impl Terminal for SharedTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        let mut state = lock_unpoisoned(&self.state);
        state.start_calls += 1;
        state.on_input = Some(on_input);
        state.on_resize = Some(on_resize);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let mut state = lock_unpoisoned(&self.state);
        state.stop_calls += 1;
        Ok(())
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
        let mut state = lock_unpoisoned(&self.state);
        state.drain_calls.push((max_ms, idle_ms));
    }

    fn write(&mut self, data: &str) {
        let mut state = lock_unpoisoned(&self.state);
        state.writes.push(data.to_string());
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }
}

pub fn inject_input(state: &Arc<Mutex<TerminalTrace>>, data: &str) {
    let mut state = lock_unpoisoned(state);
    let Some(on_input) = state.on_input.as_mut() else {
        panic!("terminal input handler is not registered");
    };

    on_input(data.to_string());
}

pub fn rendered_output(state: &Arc<Mutex<TerminalTrace>>) -> String {
    lock_unpoisoned(state).writes.join("")
}

pub fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
