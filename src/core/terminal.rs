//! Terminal trait and lifecycle helpers (Phase 1).

/// Minimal terminal interface for the TUI.
pub trait Terminal {
    /// Start the terminal with input and resize handlers.
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()>;

    /// Stop the terminal and restore state.
    fn stop(&mut self) -> std::io::Result<()>;

    /// Drain stdin before exiting to prevent key release leakage over slow connections.
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64);

    /// Write output to the terminal.
    fn write(&mut self, data: &str);

    /// Terminal dimensions.
    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
}

/// RAII guard that drains input and stops the terminal on drop.
pub struct TerminalGuard<T: Terminal> {
    terminal: Option<T>,
    max_drain_ms: u64,
    idle_drain_ms: u64,
}

impl<T: Terminal> TerminalGuard<T> {
    /// Create a guard with default drain timings (max 1000ms, idle 50ms).
    pub fn new(terminal: T) -> Self {
        Self {
            terminal: Some(terminal),
            max_drain_ms: 1000,
            idle_drain_ms: 50,
        }
    }

    /// Adjust drain timings.
    pub fn set_drain_timings(&mut self, max_ms: u64, idle_ms: u64) {
        self.max_drain_ms = max_ms;
        self.idle_drain_ms = idle_ms;
    }

    /// Access the wrapped terminal.
    pub fn terminal_mut(&mut self) -> &mut T {
        self.terminal
            .as_mut()
            .expect("terminal already taken from guard")
    }

    /// Consume the guard without running cleanup.
    pub fn into_inner(mut self) -> T {
        self.terminal
            .take()
            .expect("terminal already taken from guard")
    }
}

impl<T: Terminal> Drop for TerminalGuard<T> {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.drain_input(self.max_drain_ms, self.idle_drain_ms);
            let _ = terminal.stop();
        }
    }
}
