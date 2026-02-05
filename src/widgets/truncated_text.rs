//! Truncated text widget (Phase 18).

use crate::core::component::Component;
use crate::render::utils::truncate_to_width;
use crate::render::width::visible_width;

pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }
}

impl Component for TruncatedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut result = Vec::new();
        let empty_line = " ".repeat(width);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        let available_width = width.saturating_sub(self.padding_x * 2).max(1);

        let mut single_line_text = self.text.as_str();
        if let Some(newline_index) = self.text.find('\n') {
            single_line_text = &self.text[..newline_index];
        }

        let display_text = truncate_to_width(single_line_text, available_width, "...", false);

        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");

        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible_width);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));

        result.push(final_line);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    fn invalidate(&mut self) {
        // No cached state.
    }
}

#[cfg(test)]
mod tests {
    use super::TruncatedText;
    use crate::core::component::Component;
    use crate::render::width::visible_width;

    #[test]
    fn truncated_text_truncates_with_ellipsis() {
        let mut text = TruncatedText::new("hello world", 0, 0);
        let lines = text.render(8);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("..."));
        assert_eq!(visible_width(&lines[0]), 8);
    }

    #[test]
    fn truncated_text_respects_padding() {
        let mut text = TruncatedText::new("hi", 1, 1);
        let lines = text.render(4);
        assert_eq!(lines, vec!["    ", " hi ", "    "]);
    }
}
