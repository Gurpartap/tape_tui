//! Component and Focusable traits (Phase 5).

use crate::core::input_event::InputEvent;

/// Renderable component interface.
pub trait Component {
    /// Render to a list of lines at the given width.
    fn render(&mut self, width: usize) -> Vec<String>;

    /// Handle input events (raw key sequences).
    fn handle_input(&mut self, _data: &str) {}

    /// Handle input events (structured).
    ///
    /// Prefer overriding this method instead of `handle_input`.
    fn handle_event(&mut self, event: &InputEvent) {
        self.handle_input(&event.raw)
    }

    /// Invalidate any cached state.
    fn invalidate(&mut self) {}

    /// Provide the current terminal row count (optional).
    fn set_terminal_rows(&mut self, _rows: usize) {}

    /// Whether this component wants key-release events.
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Optional focusable behavior for IME cursor handling.
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        None
    }
}

/// Focusable behavior for components that track focus.
pub trait Focusable {
    fn set_focused(&mut self, focused: bool);
    fn is_focused(&self) -> bool;
}
