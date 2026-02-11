//! Editor component interface.

use crate::core::autocomplete::AutocompleteProvider;
use crate::core::component::Component;

/// Interface for editor components with optional advanced capabilities.
pub trait EditorComponent: Component {
    /// Get the current text content.
    fn get_text(&self) -> String;

    /// Set the text content.
    fn set_text(&mut self, text: &str);

    /// Set submit handler.
    fn set_on_submit(&mut self, _handler: Option<Box<dyn FnMut(String)>>) {}

    /// Set change handler.
    fn set_on_change(&mut self, _handler: Option<Box<dyn FnMut(String)>>) {}

    /// Add text to history for up/down navigation.
    fn add_to_history(&mut self, _text: &str) {}

    /// Insert text at cursor position (optional).
    fn insert_text_at_cursor(&mut self, _text: &str) {}

    /// Get expanded text (e.g., paste markers expanded).
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    /// Set autocomplete provider (optional).
    fn set_autocomplete_provider(&mut self, _provider: Box<dyn AutocompleteProvider>) {}

    /// Set border color function (optional).
    fn set_border_color(&mut self, _border_color: Box<dyn Fn(&str) -> String>) {}

    /// Set horizontal padding (optional).
    fn set_padding_x(&mut self, _padding: usize) {}

    /// Get horizontal padding (optional).
    fn get_padding_x(&self) -> usize {
        0
    }

    /// Set max visible autocomplete rows (optional).
    fn set_autocomplete_max_visible(&mut self, _max_visible: usize) {}

    /// Get max visible autocomplete rows (optional).
    fn get_autocomplete_max_visible(&self) -> usize {
        5
    }
}
