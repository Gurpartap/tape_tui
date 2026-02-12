//! Component and Focusable traits.

use crate::core::input_event::InputEvent;

/// Renderable component interface.
pub trait Component {
    /// Render to a list of lines at the given width.
    fn render(&mut self, width: usize) -> Vec<String>;

    /// Provide an allocated viewport size for this component (optional).
    ///
    /// This is intended for surface components that need to size nested terminal
    /// emulation (for example, a PTY-backed TUI) to match the space the surface
    /// is allowed to use.
    ///
    /// This is a constraint/budget, not a promise about the number of lines that
    /// will be rendered.
    fn set_viewport_size(&mut self, _cols: usize, _rows: usize) {}

    /// Handle input events.
    fn handle_event(&mut self, _event: &InputEvent) {}

    /// Optional cursor position metadata for this component's last render.
    ///
    /// The cursor position is relative to the lines returned from `render()`.
    fn cursor_pos(&self) -> Option<crate::core::cursor::CursorPos> {
        None
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
